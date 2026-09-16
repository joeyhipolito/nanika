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
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::unix::net::UnixStream,
    time::Duration,
};
use support::{FixtureRoot, event};

fn request(address: SocketAddr, bytes: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(bytes)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    Ok(response)
}

fn sse_until(
    address: SocketAddr,
    request: &[u8],
    marker: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(request)?;
    let mut response = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err("SSE stream closed before expected event".into());
        }
        response.extend_from_slice(&chunk[..read]);
        if response
            .windows(marker.len())
            .any(|window| window == marker)
        {
            return Ok(response);
        }
    }
}

#[test]
fn authenticated_uds_loopback_http_and_sse_share_exact_ordered_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = FixtureRoot::new("daemon-loopback")?;
    let mut daemon = fixture.start_daemon("fixture-secret")?;
    let identity = fixture.wait_identity(Duration::from_secs(2))?;
    let address = SocketAddr::from(([127, 0, 0, 1], identity.port));

    let health = request(
        address,
        b"GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    )?;
    assert!(health.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(health.ends_with(b"{\"status\":\"ok\"}\n"));
    let denied = request(
        address,
        b"GET /api/events HTTP/1.1\r\nHost: localhost\r\n\r\n",
    )?;
    assert!(denied.starts_with(b"HTTP/1.1 401 Unauthorized\r\n"));

    let first = event("evt_0000000000000001", 1, "fixture-mission")?;
    let mut unauthenticated = UnixStream::connect(fixture.root.join("daemon.sock"))?;
    unauthenticated.set_read_timeout(Some(Duration::from_secs(2)))?;
    unauthenticated.write_all(b"{\"token\":\"wrong\"}\n")?;
    unauthenticated.write_all(&first)?;
    unauthenticated.write_all(b"\n")?;
    let mut rejection = [0u8; 128];
    let rejected = unauthenticated.read(&mut rejection)?;
    assert!(String::from_utf8_lossy(&rejection[..rejected]).contains("unauthorized"));

    let client = DaemonClient::open(&fixture.root)?;
    client.emit(&first)?;
    let sse = sse_until(
        address,
        b"GET /api/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer fixture-secret\r\nLast-Event-ID: 0\r\n\r\n",
        &first,
    )?;
    assert!(sse.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(sse.windows(6).any(|window| window == b"id: 1\n"));

    let second = event("evt_0000000000000002", 2, "fixture-mission")?;
    let mut local = client.subscribe(1)?;
    local.set_read_timeout(Some(Duration::from_secs(2)))?;
    let request_head = format!(
        "POST /api/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer fixture-secret\r\nContent-Length: {}\r\n\r\n",
        second.len()
    );
    let mut request_bytes = request_head.into_bytes();
    request_bytes.extend_from_slice(&second);
    let accepted = request(address, &request_bytes)?;
    assert!(accepted.starts_with(b"HTTP/1.1 202 Accepted\r\n"));
    let delivered = local
        .read_event()?
        .ok_or("HTTP event was not broadcast to UDS")?;
    assert_eq!(delivered.cursor, 2);
    assert_eq!(delivered.ingress_bytes, second);

    fixture.stop()?;
    daemon.wait_stopped()?;
    fixture.assert_no_daemon_leaks();
    Ok(())
}
