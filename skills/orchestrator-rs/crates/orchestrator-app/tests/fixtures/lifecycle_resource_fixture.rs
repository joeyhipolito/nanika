use orchestrator_app::{
    CancellationToken, FixtureAdmissionPolicy, FixtureProcessAuthority, FixtureProcessReport,
    FixtureProcessSpec, FixtureWorkspaceSeed, FreshFixtureAuthority, IsolatedFixtureRoot,
    ProcessTermination, SupervisorLimits,
};
use orchestrator_core::{CheckpointProjection, MissionId};
use rustix::process::setpgid;
use serde_json::json;
use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

type ProbeResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Arguments {
    parent: PathBuf,
    helper: PathBuf,
    control: PathBuf,
    cycles: u32,
}

fn arguments() -> ProbeResult<Arguments> {
    let mut parent = None;
    let mut helper = None;
    let mut control = None;
    let mut cycles = None;
    let mut values = env::args().skip(1);
    while let Some(argument) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| format!("missing value for {argument}"))?;
        match argument.as_str() {
            "--parent" => parent = Some(PathBuf::from(value)),
            "--helper" => helper = Some(PathBuf::from(value)),
            "--control" => control = Some(PathBuf::from(value)),
            "--cycles" => cycles = Some(value.parse::<u32>()?),
            _ => return Err(format!("unknown argument {argument}").into()),
        }
    }
    let cycles = cycles.ok_or("missing --cycles")?;
    if cycles != 100 {
        return Err("the evidence probe requires exactly 100 cycles".into());
    }
    Ok(Arguments {
        parent: parent.ok_or("missing --parent")?,
        helper: helper.ok_or("missing --helper")?,
        control: control.ok_or("missing --control")?,
        cycles,
    })
}

fn emit(value: serde_json::Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    writeln!(stdout)?;
    stdout.flush()
}

fn stage(control: &Path, name: &str) -> ProbeResult {
    let stage = control.join(format!("stage-{name}"));
    let acknowledgement = control.join(format!("ack-{name}"));
    fs::write(&stage, b"ready\n")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !acknowledgement.is_file() {
        if Instant::now() >= deadline {
            return Err(format!("timed out awaiting acknowledgement for {name}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn controlled_spec(
    mode: &str,
    control: &Path,
    generation: u32,
    timeout: Duration,
) -> ProbeResult<FixtureProcessSpec> {
    Ok(FixtureProcessSpec::new(
        [
            OsString::from(mode),
            control.as_os_str().to_os_string(),
            OsString::from(generation.to_string()),
        ],
        timeout,
    )?)
}

fn observed_owned_group(control: &Path, generation: u32, timeout: Duration) -> ProbeResult<u32> {
    let prefix = format!("owned-group-{generation}-");
    let deadline = Instant::now() + timeout;
    loop {
        let mut observed = None;
        for entry in fs::read_dir(control)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(raw) = name.strip_prefix(&prefix) else {
                continue;
            };
            let pgid = raw.parse::<u32>()?;
            if pgid == 0 || observed.replace(pgid).is_some() {
                return Err(format!(
                    "generation {generation} did not identify exactly one owned process group"
                )
                .into());
            }
        }
        if let Some(pgid) = observed {
            let acknowledgement = control.join(format!("ack-owned-group-{generation}-{pgid}"));
            match fs::symlink_metadata(&acknowledgement) {
                Ok(metadata) if metadata.file_type().is_file() => return Ok(pgid),
                Ok(_) => {
                    return Err(format!(
                        "owned process-group acknowledgement is not a regular file for {pgid}"
                    )
                    .into());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "external monitor did not prove a live owned group for generation {generation}"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn run_observed(
    authority: &FixtureProcessAuthority,
    fixture: &FixtureProcessSpec,
    control: &Path,
    generation: u32,
) -> ProbeResult<(FixtureProcessReport, Duration)> {
    thread::scope(|scope| -> ProbeResult<(FixtureProcessReport, Duration)> {
        let runner = scope.spawn(|| authority.run(fixture));
        let observed_pgid = observed_owned_group(control, generation, Duration::from_secs(5))?;
        let finish_started = Instant::now();
        let report = runner
            .join()
            .map_err(|_| io::Error::other("production process runner panicked"))??;
        let reported_pgid = report
            .process
            .pgid
            .ok_or("production process receipt omitted its group ID")?;
        if reported_pgid != observed_pgid {
            return Err(format!(
                "live group {observed_pgid} did not match receipt group {reported_pgid}"
            )
            .into());
        }
        Ok((report, finish_started.elapsed()))
    })
}

fn require_clean_tree(report: &FixtureProcessReport, label: &str) -> ProbeResult<u32> {
    if !report.is_success()
        || !report.process.direct_child_reaped
        || !report.process.group_absent
        || !report.process.cleanup_complete
        || report.protocol.logical_children_started != 2
        || report.protocol.logical_children_reaped != 2
    {
        return Err(format!("{label} did not cleanly supervise its worker tree").into());
    }
    report
        .process
        .pgid
        .ok_or_else(|| format!("{label} receipt omitted its group ID").into())
}

fn main() -> ProbeResult {
    setpgid(None, None)?;
    let arguments = arguments()?;
    let parent = fs::canonicalize(&arguments.parent)?;
    let control = fs::canonicalize(&arguments.control)?;
    let helper = fs::canonicalize(&arguments.helper)?;
    let temporary = fs::canonicalize(env::temp_dir())?;
    let checkout = fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let helper_bytes = fs::read(&helper)?;

    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let fixture_root = isolated.path().to_path_buf();
    let live_user = parent.join("live-user-sentinel-root");
    private_dir(&live_user)?;
    let sentinel = live_user.join("sentinel");
    fs::write(&sentinel, b"live-home-sentinel\n")?;
    let policy = FixtureAdmissionPolicy::new(&live_user, checkout, temporary)
        .with_runtime_home_candidate(live_user.join(".alluka"))
        .with_runtime_home_candidate(live_user.join(".via"))
        .with_expected_fixture_helper(&helper_bytes);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let checkpoint = CheckpointProjection {
        workspace_id: "synthetic-supervision-resource".to_owned(),
        status: "pending".to_owned(),
        started_at: "2026-07-14T00:00:00Z".to_owned(),
        ..CheckpointProjection::default()
    };
    let workspace = authority.create_workspace(
        MissionId::new("synthetic-supervision-resource")?,
        FixtureWorkspaceSeed::new(b"synthetic fixture\n".to_vec(), &checkpoint, b"{}".to_vec())?,
    )?;
    let executable = authority.install_fixture_executable("native-helper", &helper_bytes)?;
    let limits =
        SupervisorLimits::for_tests(Duration::from_millis(100), Duration::from_millis(500))?;
    let process_authority = FixtureProcessAuthority::new(executable, &workspace, limits)?;

    for warmup in 1..=3 {
        let fixture = controlled_spec("tree-controlled", &control, warmup, Duration::from_secs(3))?;
        let (report, _) = run_observed(&process_authority, &fixture, &control, warmup)?;
        require_clean_tree(&report, &format!("warmup cycle {warmup}"))?;
    }

    emit(json!({
        "event": "ready",
        "cycles": arguments.cycles,
        "warmup_cycles": 3,
        "fixture_root": fixture_root.file_name().and_then(|name| name.to_str()),
    }))?;
    stage(&control, "ready")?;

    let milestones = [1_u32, 10, 25, 50, 75, 100];
    let mut pgids = Vec::with_capacity(arguments.cycles as usize + 1);
    let mut maximum_cycle_finish_ms = 0_u128;
    for cycle in 1..=arguments.cycles {
        let generation = 3 + cycle;
        let fixture = controlled_spec(
            "tree-controlled",
            &control,
            generation,
            Duration::from_secs(3),
        )?;
        let (report, finish_elapsed) =
            run_observed(&process_authority, &fixture, &control, generation)?;
        maximum_cycle_finish_ms = maximum_cycle_finish_ms.max(finish_elapsed.as_millis());
        pgids.push(require_clean_tree(&report, &format!("cycle {cycle}"))?);
        if milestones.contains(&cycle) {
            emit(json!({"event": "milestone", "cycle": cycle}))?;
            stage(&control, &format!("cycle-{cycle}"))?;
        }
    }

    let cleanup_generation = 3 + arguments.cycles + 1;
    let token = CancellationToken::new();
    let cleanup_fixture = controlled_spec(
        "hang-tree-controlled",
        &control,
        cleanup_generation,
        Duration::from_secs(10),
    )?
    .with_cancellation(token.clone());
    let (cleanup_report, observed_cleanup_pgid, cleanup_latency_ms) =
        thread::scope(|scope| -> ProbeResult<_> {
            let runner = scope.spawn(|| process_authority.run(&cleanup_fixture));
            let observed_pgid =
                observed_owned_group(&control, cleanup_generation, Duration::from_secs(5))?;
            emit(json!({
                "event": "active-tree",
                "pgid": observed_pgid,
            }))?;
            stage(&control, "active-tree")?;
            let cleanup_started = Instant::now();
            if !token.cancel() {
                return Err("cleanup cancellation token was already cancelled".into());
            }
            let report = runner
                .join()
                .map_err(|_| io::Error::other("production cleanup runner panicked"))??;
            Ok((report, observed_pgid, cleanup_started.elapsed().as_millis()))
        })?;
    let cleanup_pgid = cleanup_report
        .process
        .pgid
        .ok_or("cleanup receipt omitted its process-group ID")?;
    if cleanup_pgid != observed_cleanup_pgid
        || cleanup_report.process.termination != ProcessTermination::Cancelled
        || !cleanup_report.process.cancellation_observed
        || !cleanup_report.process.term_sent
        || !cleanup_report.process.direct_child_reaped
        || !cleanup_report.process.group_absent
        || !cleanup_report.process.cleanup_complete
        || cleanup_report.protocol.logical_children_started != 1
    {
        return Err("cancellation cleanup probe left unresolved ownership".into());
    }
    pgids.push(cleanup_pgid);
    if process_authority.has_unresolved_processes() {
        return Err("process registry remained unresolved after campaign".into());
    }
    if fs::read(&sentinel)? != b"live-home-sentinel\n" {
        return Err("live-home sentinel changed".into());
    }

    emit(json!({
        "event": "campaign-complete",
        "cycles": arguments.cycles,
        "warmup_cycles": 3,
        "worker_trees": arguments.cycles,
        "descendants_started": arguments.cycles * 6,
        "descendants_reaped": arguments.cycles * 6,
        "supervised_pgids": pgids,
        "maximum_cycle_finish_ms": maximum_cycle_finish_ms,
        "cleanup_latency_ms": cleanup_latency_ms,
        "registry_unresolved": false,
        "live_home_sentinel_unchanged": true,
    }))?;
    stage(&control, "post")?;
    stage(&control, "finish")?;
    Ok(())
}
