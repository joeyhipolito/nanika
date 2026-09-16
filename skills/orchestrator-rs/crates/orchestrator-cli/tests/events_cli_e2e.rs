mod support {
    #![allow(dead_code)]

    use orchestrator_app::{FixtureAdmissionPolicy, FreshFixtureAuthority, IsolatedFixtureRoot};
    use orchestrator_core::{EventJsonMap, EventRecord, encode_current_event};
    use orchestrator_daemon::{DaemonClient, DaemonIdentity};
    use std::{
        fs, io,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::{Child, Command, Output, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    static NEXT: AtomicU64 = AtomicU64::new(1);

    pub struct FixtureRoot {
        parent: PathBuf,
        pub root: PathBuf,
        pub fake_home: PathBuf,
        pub checkout: PathBuf,
    }

    impl FixtureRoot {
        pub fn new(_label: &str) -> Result<Self, Box<dyn std::error::Error>> {
            // Keep the fixture beneath the process temporary directory (required
            // by fixture admission) while minimizing the leaf: macOS's canonical
            // per-user temp prefix is close to sockaddr_un's portable ceiling.
            let parent =
                std::env::temp_dir().join(format!("b{:x}", NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&parent)?;
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
            let fake_home = parent.join("live-home-sentinel");
            let checkout = parent.join("checkout");
            fs::create_dir(&fake_home)?;
            fs::create_dir(&checkout)?;
            let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
            let root = isolated.path().to_path_buf();
            let policy = FixtureAdmissionPolicy::new(&fake_home, &checkout, &parent);
            let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
            drop(authority);
            Ok(Self {
                parent,
                root,
                fake_home,
                checkout,
            })
        }

        pub fn command(&self) -> Command {
            let mut command = Command::new(env!("CARGO_BIN_EXE_orchestrator"));
            command
                .env_clear()
                .env("HOME", &self.fake_home)
                .env("ALLUKA_HOME", &self.root)
                .env("TMPDIR", &self.parent)
                .env("PATH", "/usr/bin:/bin")
                .current_dir(&self.checkout);
            command
        }

        pub fn start_daemon(&self, key: &str) -> Result<DaemonProcess, Box<dyn std::error::Error>> {
            let child = self
                .command()
                .args(["daemon", "start", "--port", "0", "--api-key", key])
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()?;
            let process = DaemonProcess { child: Some(child) };
            self.wait_identity(Duration::from_secs(5))?;
            Ok(process)
        }

        pub fn wait_identity(
            &self,
            timeout: Duration,
        ) -> Result<DaemonIdentity, Box<dyn std::error::Error>> {
            let started = Instant::now();
            loop {
                if let Ok(client) = DaemonClient::open(&self.root) {
                    if let Some(identity) = client.identity()? {
                        return Ok(identity);
                    }
                }
                if started.elapsed() >= timeout {
                    return Err("daemon did not become ready".into());
                }
                std::thread::park_timeout(Duration::from_millis(20));
            }
        }

        pub fn run(&self, arguments: &[&str]) -> io::Result<Output> {
            self.command().args(arguments).output()
        }

        pub fn stop(&self) -> Result<Output, Box<dyn std::error::Error>> {
            let output = self.run(&["daemon", "stop"])?;
            if !output.status.success() {
                return Err(format!(
                    "daemon stop failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }
            Ok(output)
        }

        pub fn assert_no_daemon_leaks(&self) {
            for name in [
                "daemon.sock",
                "events.sock",
                "daemon.pid",
                "daemon.identity.json",
                "daemon.auth",
            ] {
                assert!(
                    !self.root.join(name).exists(),
                    "leaked daemon artifact {name}"
                );
            }
        }
    }

    impl Drop for FixtureRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }

    pub struct DaemonProcess {
        child: Option<Child>,
    }

    impl DaemonProcess {
        pub fn wait_stopped(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            let started = Instant::now();
            loop {
                let Some(child) = self.child.as_mut() else {
                    return Ok(());
                };
                if let Some(status) = child.try_wait()? {
                    self.child.take();
                    if status.success() {
                        return Ok(());
                    }
                    return Err(format!("daemon exited with {status}").into());
                }
                if started.elapsed() >= Duration::from_secs(5) {
                    return Err("daemon did not stop".into());
                }
                std::thread::park_timeout(Duration::from_millis(20));
            }
        }
    }

    impl Drop for DaemonProcess {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    pub fn event(
        id: &str,
        sequence: i64,
        mission: &str,
    ) -> Result<Vec<u8>, orchestrator_core::EventError> {
        encode_current_event(&EventRecord {
            id: id.to_owned(),
            event_type: "worker.output".into(),
            timestamp: "2026-07-22T00:00:00Z".into(),
            sequence,
            mission_id: mission.into(),
            phase_id: None,
            worker_id: None,
            data: Some(EventJsonMap::default()),
            extra: EventJsonMap::default(),
        })
    }

    pub fn assert_private(path: &Path, expected: u32) -> io::Result<()> {
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, expected);
        Ok(())
    }
}

use orchestrator_daemon::DaemonClient;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use support::{FixtureRoot, assert_private, event};

#[test]
fn events_cli_attaches_exactly_and_legacy_process_state_is_never_targeted()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureRoot::new("events-cli")?;
    let live_sentinel = fixture.fake_home.join("never-open-this-provider");
    fs::write(&live_sentinel, b"unchanged")?;
    let help = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .env_clear()
        .args(["daemon", "--help"])
        .output()?;
    assert!(help.status.success());
    let help_text = String::from_utf8(help.stdout)?;
    assert!(help_text.contains("orchestrator daemon [command]"));
    assert!(help_text.contains("unsupported"));
    let start_help = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .env_clear()
        .args(["help", "daemon", "start"])
        .output()?;
    assert!(start_help.status.success());
    assert!(String::from_utf8(start_help.stdout)?.contains("--port int"));

    let mission = "fixture-mission";
    let first = event("evt_0000000000000001", 1, mission)?;
    let second = event("evt_0000000000000002", 2, mission)?;
    let mut daemon = fixture.start_daemon("fixture-secret")?;
    let client = DaemonClient::open(&fixture.root)?;
    client.emit(&first)?;

    // A direct .jsonl argument remains authoritative even while the daemon is
    // live. The daemon only owns the runtime home's canonical mission logs.
    let direct_path = fixture
        .root
        .parent()
        .ok_or("fixture root has no parent")?
        .join("direct-events.jsonl");
    fs::write(&direct_path, b"")?;
    let direct_path_text = direct_path.to_str().ok_or("direct path is not UTF-8")?;
    let mut direct_tail = fixture
        .command()
        .args(["events", "tail", direct_path_text, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let direct_stderr = direct_tail
        .stderr
        .take()
        .ok_or("direct tail stderr unavailable")?;
    let mut direct_readiness = String::new();
    BufReader::new(direct_stderr).read_line(&mut direct_readiness)?;
    assert!(direct_readiness.contains("tailing direct-events.jsonl"));
    let direct_stdout = direct_tail
        .stdout
        .take()
        .ok_or("direct tail stdout unavailable")?;
    let (direct_tx, direct_rx) = mpsc::channel();
    let direct_reader = std::thread::spawn(move || {
        let mut line = Vec::new();
        let result = BufReader::new(direct_stdout)
            .read_until(b'\n', &mut line)
            .map(|_| line);
        let _ = direct_tx.send(result);
    });
    let direct_event = event("evt_0000000000000001", 1, "direct-mission")?;
    let mut direct_file = fs::OpenOptions::new().append(true).open(&direct_path)?;
    direct_file.write_all(&direct_event)?;
    direct_file.write_all(b"\n")?;
    direct_file.sync_all()?;
    let direct_line = direct_rx.recv_timeout(Duration::from_secs(3))??;
    assert_eq!(direct_line, [direct_event.as_slice(), b"\n"].concat());
    direct_tail.kill()?;
    direct_tail.wait()?;
    direct_reader
        .join()
        .map_err(|_| "direct tail reader panicked")?;

    let status = fixture.run(&["daemon", "status", "ignored-like-go"])?;
    assert!(status.status.success());
    assert!(String::from_utf8(status.stdout)?.contains("daemon: running (PID "));
    for unsupported in [
        &["daemon", "backfill-vectors"][..],
        &["daemon", "start", "--dashboard", "/tmp/assets"][..],
    ] {
        let output = fixture.run(unsupported)?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported"));
    }

    let mut tail = fixture
        .command()
        .args(["events", "tail", mission, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = tail.stderr.take().ok_or("tail stderr unavailable")?;
    let mut readiness = String::new();
    BufReader::new(stderr).read_line(&mut readiness)?;
    assert!(readiness.contains("tailing fixture-mission.jsonl"));

    let stdout = tail.stdout.take().ok_or("tail stdout unavailable")?;
    let (line_tx, line_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = Vec::new();
        let result = BufReader::new(stdout)
            .read_until(b'\n', &mut line)
            .map(|_| line);
        let _ = line_tx.send(result);
    });
    // Crash after the initial subscription acknowledgement but before its
    // first live event. Reconnect must resume from the acknowledged durable
    // high-water and deliver the next commit exactly once.
    fixture.stop()?;
    daemon.wait_stopped()?;
    daemon = fixture.start_daemon("fixture-secret")?;
    let client = DaemonClient::open(&fixture.root)?;
    client.emit(&second)?;
    let tailed = line_rx.recv_timeout(Duration::from_secs(3))??;
    assert_eq!(tailed, [second.as_slice(), b"\n"].concat());
    tail.kill()?;
    tail.wait()?;
    reader.join().map_err(|_| "tail reader panicked")?;

    fixture.stop()?;
    daemon.wait_stopped()?;
    let replay = fixture.run(&["events", "replay", mission, "--json"])?;
    assert!(replay.status.success());
    assert_eq!(
        replay.stdout,
        [first.as_slice(), b"\n", second.as_slice(), b"\n"].concat()
    );
    let list = fixture.run(&["events", "list"])?;
    assert!(list.status.success());
    let list_text = String::from_utf8(list.stdout)?;
    assert!(list_text.contains(mission));
    assert!(list_text.contains('2'));

    let event_path = fixture.root.join("events").join(format!("{mission}.jsonl"));
    assert_private(&fixture.root, 0o700)?;
    assert_private(&fixture.root.join("events"), 0o700)?;
    assert_private(&event_path, 0o600)?;

    // Exercise the exact replay boundary cases in the B1 matrix through the
    // shipped CLI: corrupt source bytes stay visible, an unknown type/data
    // envelope formats without losing fields, and a final token without LF is
    // emitted once with the same Go scanner newline normalization.
    let unknown = br#"{"id":"evt_0000000000000003","type":"future.unknown","timestamp":"2026-07-22T00:00:02Z","sequence":3,"mission_id":"fixture-mission","data":{"alpha":"value","nested":[1,true,null]}}"#;
    let mut edge_bytes = fs::read(&event_path)?;
    edge_bytes.extend_from_slice(b"not json\n");
    edge_bytes.extend_from_slice(unknown);
    fs::write(&event_path, &edge_bytes)?;
    let edge_raw = fixture.run(&["events", "replay", mission, "--json"])?;
    assert!(edge_raw.status.success());
    assert_eq!(
        edge_raw.stdout,
        [edge_bytes.as_slice(), b"\n"].concat(),
        "raw replay changed corrupt/final-no-LF source order"
    );
    let edge_formatted = fixture.run(&["events", "replay", mission])?;
    assert!(edge_formatted.status.success());
    let edge_text = String::from_utf8(edge_formatted.stdout)?;
    assert!(edge_text.contains("not json"));
    assert!(edge_text.contains("future.unknown"));
    assert!(edge_text.contains("alpha=value"));
    assert!(edge_text.contains("nested=[1 true <nil>]"));

    // Go's replay scanner accepts at most 64 KiB - 1 content bytes. Prove the
    // largest token succeeds and the first oversized token makes the command
    // fail instead of returning a partial PASS.
    let boundary_mission = "replay-size-boundary";
    let boundary_path = fixture
        .root
        .join("events")
        .join(format!("{boundary_mission}.jsonl"));
    let mut largest = vec![b'x'; 64 * 1024 - 1];
    largest.push(b'\n');
    fs::write(&boundary_path, &largest)?;
    let boundary = fixture.run(&["events", "replay", boundary_mission, "--json"])?;
    assert!(boundary.status.success());
    assert_eq!(boundary.stdout, largest);
    fs::write(&boundary_path, vec![b'x'; 64 * 1024])?;
    let oversized = fixture.run(&["events", "replay", boundary_mission, "--json"])?;
    assert!(!oversized.status.success());
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("exceeds the 64 KiB"));

    fs::write(
        fixture.root.join("daemon.pid"),
        format!("{}\n", std::process::id()),
    )?;
    let legacy_status = fixture.run(&["daemon", "status"])?;
    assert!(legacy_status.status.success());
    assert!(String::from_utf8(legacy_status.stdout)?.contains("Rust will not target it"));
    let legacy_stop = fixture.run(&["daemon", "stop"])?;
    assert!(!legacy_stop.status.success());
    assert!(String::from_utf8_lossy(&legacy_stop.stderr).contains("stale or ambiguous"));
    fs::remove_file(fixture.root.join("daemon.pid"))?;

    assert_eq!(fs::read(&live_sentinel)?, b"unchanged");
    fixture.assert_no_daemon_leaks();
    Ok(())
}
