use rustix::process::{Signal, getpgid, getpid, kill_process};
use serde_json::json;
use std::{
    env,
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{self, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const DURABLE_CANARY_SENTINEL: &str = ".nanika-durable-canary-launched";
const DURABLE_CANARY_DUPLICATE: &str = ".nanika-durable-canary-duplicate";
const DURABLE_CANARY_CONTENT: &[u8] = b"nanika-durable-canary-v1\n";

fn frame(seq: &mut u64, kind: &str, data: serde_json::Value) -> io::Result<()> {
    *seq = seq.saturating_add(1);
    let value = json!({"version": 1, "seq": *seq, "kind": kind, "data": data});
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "ORCHESTRATOR_FIXTURE_V1 {value}")?;
    stderr.flush()
}

fn done(seq: &mut u64, started: u64, reaped: u64) -> io::Result<()> {
    frame(
        seq,
        "done",
        json!({"children_started": started, "children_reaped": reaped}),
    )
}

fn spawn_self(arguments: &[&str]) -> io::Result<process::Child> {
    let executable = env::current_exe()?;
    Command::new(executable)
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn wait_for_child(child: &mut process::Child) -> io::Result<process::ExitStatus> {
    child.wait()
}

fn tree_node(depth: u32) -> io::Result<()> {
    if depth == 0 {
        return Ok(());
    }
    let next = (depth - 1).to_string();
    let mut child = spawn_self(&["tree-node", &next])?;
    let _status = wait_for_child(&mut child)?;
    Ok(())
}

fn announce_owned_group(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let control = Path::new(
        arguments
            .get(1)
            .ok_or("controlled fixture mode requires a control directory")?,
    );
    let generation = arguments
        .get(2)
        .ok_or("controlled fixture mode requires a generation")?
        .parse::<u32>()?;
    if generation == 0 {
        return Err("controlled fixture generation must be non-zero".into());
    }
    let pgid = getpgid(None)?.as_raw_nonzero().get();
    let marker = control.join(format!("owned-group-{generation}-{pgid}"));
    let acknowledgement = control.join(format!("ack-owned-group-{generation}-{pgid}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)?;
    file.write_all(b"owned\n")?;
    file.sync_all()?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while !acknowledgement.is_file() {
        if Instant::now() >= deadline {
            return Err(
                format!("external monitor did not observe owned process group {pgid}").into(),
            );
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

fn verification(
    seq: &mut u64,
    scenario: &str,
    discovered: u64,
    executed: u64,
    passed: u64,
    failed: u64,
    required_skipped: u64,
) -> io::Result<()> {
    frame(
        seq,
        "verification",
        json!({
            "scenario": scenario,
            "discovered": discovered,
            "executed": executed,
            "passed": passed,
            "failed": failed,
            "required_skipped": required_skipped,
        }),
    )
}

fn durable_canary(seq: &mut u64) -> Result<(), Box<dyn std::error::Error>> {
    let working_directory = env::current_dir()?;
    let sentinel = working_directory.join(DURABLE_CANARY_SENTINEL);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(sentinel)
    {
        Ok(mut file) => {
            file.write_all(DURABLE_CANARY_CONTENT)?;
            file.sync_all()?;
            std::fs::File::open(&working_directory)?.sync_all()?;
            done(seq, 0, 0)?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let mut duplicate = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(working_directory.join(DURABLE_CANARY_DUPLICATE))?;
            duplicate.write_all(DURABLE_CANARY_CONTENT)?;
            duplicate.sync_all()?;
            std::fs::File::open(&working_directory)?.sync_all()?;
            done(seq, 0, 0)?;
            process::exit(71);
        }
        Err(error) => Err(error.into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let mode = arguments.first().map(String::as_str).unwrap_or("zero");
    if mode == "tree-node" {
        let depth = arguments
            .get(1)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        tree_node(depth)?;
        return Ok(());
    }
    if mode == "sleep" {
        thread::sleep(Duration::from_secs(60));
        return Ok(());
    }

    let mut seq = 0;
    frame(&mut seq, "ready", json!({}))?;
    match mode {
        "zero" => done(&mut seq, 0, 0)?,
        "durable-canary" => durable_canary(&mut seq)?,
        "inspect" => {
            let selected_environment = env::vars()
                .filter(|(name, _)| {
                    matches!(
                        name.as_str(),
                        "HOME"
                            | "TMPDIR"
                            | "PATH"
                            | "LANG"
                            | "LC_ALL"
                            | "ORCHESTRATOR_FIXTURE_PROTOCOL"
                            | "ORCHESTRATOR_POISON"
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "arguments": arguments,
                    "environment": selected_environment,
                    "cwd": env::current_dir()?,
                }))?
            );
            done(&mut seq, 0, 0)?;
        }
        "nonzero" => {
            done(&mut seq, 0, 0)?;
            process::exit(7);
        }
        "signal" => {
            done(&mut seq, 0, 0)?;
            kill_process(getpid(), Signal::USR1)?;
            thread::sleep(Duration::from_secs(1));
        }
        "missing-final" => {}
        "verification-pass" => {
            verification(&mut seq, "pass", 2, 2, 2, 0, 0)?;
            done(&mut seq, 0, 0)?;
        }
        "verification-fail" => {
            verification(&mut seq, "fail", 2, 2, 1, 1, 0)?;
            done(&mut seq, 0, 0)?;
            process::exit(7);
        }
        "verification-skip" => {
            verification(&mut seq, "skip", 2, 0, 0, 0, 2)?;
            done(&mut seq, 0, 0)?;
        }
        "verification-no-tests" => {
            verification(&mut seq, "no-tests", 0, 0, 0, 0, 0)?;
            done(&mut seq, 0, 0)?;
        }
        "verification-misleading" => {
            println!("PASS: human-facing output is not the verification result");
            verification(&mut seq, "misleading", 1, 1, 0, 1, 0)?;
            done(&mut seq, 0, 0)?;
            process::exit(7);
        }
        "verification-contradictory" => {
            verification(&mut seq, "contradictory", 2, 1, 1, 0, 0)?;
            done(&mut seq, 0, 0)?;
        }
        "output" => {
            let stdout_block = vec![b'o'; 72 * 1024];
            let stderr_block = vec![b'e'; 4096];
            for _ in 0..20 {
                io::stdout().write_all(&stdout_block)?;
                io::stdout().write_all(b"\n")?;
                io::stderr().write_all(&stderr_block)?;
                io::stderr().write_all(b"\n")?;
            }
            done(&mut seq, 0, 0)?;
        }
        "periodic" => {
            for _ in 0..30 {
                thread::sleep(Duration::from_millis(50));
                frame(&mut seq, "activity", json!({}))?;
            }
            done(&mut seq, 0, 0)?;
        }
        "closed-pipes" => {
            io::stdout().flush()?;
            io::stderr().flush()?;
            let error = Command::new(env::current_exe()?)
                .arg("sleep")
                .stdin(Stdio::inherit())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .exec();
            return Err(error.into());
        }
        "descriptors" => {
            let mut descriptors = std::fs::read_dir("/dev/fd")?
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let descriptor = entry.file_name().to_string_lossy().parse::<i32>().ok()?;
                    let target = std::fs::read_link(entry.path()).ok()?;
                    Some((descriptor, target.to_string_lossy().into_owned()))
                })
                .collect::<Vec<_>>();
            descriptors.sort_unstable_by_key(|entry| entry.0);
            println!("{}", serde_json::to_string(&descriptors)?);
            done(&mut seq, 0, 0)?;
        }
        "tree" | "tree-controlled" => {
            let mut children = Vec::new();
            for index in 0..2 {
                let child = spawn_self(&["tree-node", "2"])?;
                let id = format!("child-{}", child.id());
                println!("DESCENDANT {}", child.id());
                frame(&mut seq, "child", json!({"id": id, "state": "started"}))?;
                children.push((id, child));
                let _ = index;
            }
            if mode == "tree-controlled" {
                announce_owned_group(&arguments)?;
            }
            for (id, mut child) in children {
                let _status = wait_for_child(&mut child)?;
                frame(&mut seq, "child", json!({"id": id, "state": "reaped"}))?;
            }
            done(&mut seq, 2, 2)?;
        }
        "background" => {
            let child = spawn_self(&["sleep"])?;
            let id = format!("child-{}", child.id());
            println!("DESCENDANT {}", child.id());
            frame(&mut seq, "child", json!({"id": id, "state": "started"}))?;
            done(&mut seq, 1, 0)?;
            drop(child);
        }
        "stop-tree" => {
            let child = spawn_self(&["sleep"])?;
            let id = format!("child-{}", child.id());
            println!("DESCENDANT {}", child.id());
            frame(&mut seq, "child", json!({"id": id, "state": "started"}))?;
            io::stdout().flush()?;
            io::stderr().flush()?;
            drop(child);
            kill_process(getpid(), Signal::STOP)?;
            thread::sleep(Duration::from_secs(60));
        }
        "hang-tree" | "hang-tree-controlled" => {
            let mut child = spawn_self(&["sleep"])?;
            let id = format!("child-{}", child.id());
            println!("DESCENDANT {}", child.id());
            frame(&mut seq, "child", json!({"id": id, "state": "started"}))?;
            io::stdout().flush()?;
            io::stderr().flush()?;
            if mode == "hang-tree-controlled" {
                announce_owned_group(&arguments)?;
            }
            let _status = wait_for_child(&mut child)?;
        }
        "stdin-race" => {
            let mut bytes = Vec::new();
            io::stdin().read_to_end(&mut bytes)?;
            done(&mut seq, 0, 0)?;
        }
        unknown => {
            eprintln!("unknown fixture mode: {unknown}");
            process::exit(64);
        }
    }
    io::stdout().flush()?;
    io::stderr().flush()?;
    Ok(())
}
