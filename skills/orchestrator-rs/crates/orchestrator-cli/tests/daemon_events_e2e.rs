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

use orchestrator_core::scan_event_log;
use orchestrator_daemon::DaemonClient;
use std::{fs, time::Duration};
use support::{FixtureRoot, assert_private, event};

#[test]
fn canonical_owner_reopens_reconnects_and_stops_without_leaks()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureRoot::new("daemon-events")?;
    let mission = "fixture-mission";
    let event_path = fixture.root.join("events").join(format!("{mission}.jsonl"));
    let first = event("evt_0000000000000001", 1, mission)?;
    let second = event("evt_0000000000000002", 2, mission)?;

    let mut first_daemon = fixture.start_daemon("fixture-secret")?;
    let client = DaemonClient::open(&fixture.root)?;
    let rejected = event("evt_0000000000000000", 0, "rejected-mission")?;
    assert!(client.emit(&rejected).is_err());
    assert!(
        !fixture.root.join("events").exists(),
        "rejected first ingress created or pinned a canonical mission"
    );
    let skipped = event("evt_0000000000000003", 2, "also-rejected-mission")?;
    assert!(client.emit(&skipped).is_err());
    assert!(
        !fixture.root.join("events").exists(),
        "non-successor first ingress created or pinned a canonical mission"
    );
    client.emit(&first)?;
    let mut replay = client.subscribe(0)?;
    replay.set_read_timeout(Some(Duration::from_secs(2)))?;
    let delivered = replay.read_event()?.ok_or("missing first replay event")?;
    assert_eq!(delivered.cursor, 1);
    assert_eq!(delivered.ingress_bytes, first);
    drop(replay);

    fixture.stop()?;
    first_daemon.wait_stopped()?;
    fixture.assert_no_daemon_leaks();

    let mut final_without_lf = fs::read(&event_path)?;
    assert_eq!(final_without_lf.pop(), Some(b'\n'));
    fs::write(&event_path, &final_without_lf)?;

    let mut second_daemon = fixture.start_daemon("fixture-secret")?;
    let client = DaemonClient::open(&fixture.root)?;
    let mut reconnect = client.subscribe(1)?;
    client.emit(&second)?;
    reconnect.set_read_timeout(Some(Duration::from_secs(2)))?;
    let delivered = reconnect.read_event()?.ok_or("missing reconnected event")?;
    assert_eq!(delivered.cursor, 2);
    assert_eq!(delivered.ingress_bytes, second);

    let mut expected = first.clone();
    expected.push(b'\n');
    expected.extend_from_slice(&second);
    expected.push(b'\n');
    let committed = fs::read(&event_path)?;
    assert_eq!(
        committed, expected,
        "canonical file bytes diverged from ingress"
    );
    let scan = scan_event_log(&committed);
    assert!(
        scan.diagnostics.is_empty(),
        "Go-compatible scan rejected bytes"
    );
    assert_eq!(scan.events.len(), 2);
    assert_private(&fixture.root, 0o700)?;
    assert_private(&fixture.root.join("events"), 0o700)?;
    assert_private(&event_path, 0o600)?;

    fixture.stop()?;
    second_daemon.wait_stopped()?;
    let repeated = fixture.stop()?;
    assert_eq!(String::from_utf8(repeated.stdout)?, "daemon: not running\n");
    fixture.assert_no_daemon_leaks();

    fs::write(&event_path, [committed.as_slice(), b"{corrupt\n"].concat())?;
    let corrupt_start = fixture.run(&[
        "daemon",
        "start",
        "--port",
        "0",
        "--api-key",
        "must-not-leak",
    ])?;
    assert!(!corrupt_start.status.success());
    assert!(!String::from_utf8_lossy(&corrupt_start.stderr).contains("must-not-leak"));
    fixture.assert_no_daemon_leaks();
    Ok(())
}
