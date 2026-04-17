//! Shared helpers for dust-conformance integration tests.
//!
//! `spawn_fixture()` / `spawn_and_handshake()` locate (and compile on first use)
//! the `dust-fixture-minimal` binary, then return a ready `ConformanceRunner`.
//!
//! ## Why the binary is compiled here and not in build.rs
//!
//! Running `cargo build` inside `build.rs` on the same workspace deadlocks on
//! the Cargo workspace lock.  Test functions execute AFTER compilation finishes
//! and the lock is released, so compiling from within the test setup is safe.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use dust_conformance::ConformanceRunner;

// ── Fixture binary path ───────────────────────────────────────────────────────

/// Compile `dust-fixture-minimal` on first call; return its path on all calls.
///
/// Uses `OnceLock` so the build happens at most once per test process even
/// when tests run concurrently.
fn fixture_binary() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(build_fixture)
}

fn workspace_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // dust-conformance/ → plugins/dust/ (workspace root)
    PathBuf::from(manifest_dir)
        .parent()
        .expect("CARGO_MANIFEST_DIR has no parent")
        .to_path_buf()
}

fn build_fixture() -> PathBuf {
    let root = workspace_root();

    // Always invoke `cargo build` — it's a fast no-op when nothing changed.
    // The workspace lock is released before test execution starts, so this is
    // safe to call from within a test.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(&cargo)
        .args(["build", "--bin", "dust-fixture-minimal"])
        .current_dir(&root)
        .status()
        .expect("failed to invoke cargo to build dust-fixture-minimal");

    assert!(
        status.success(),
        "cargo build --bin dust-fixture-minimal failed"
    );

    let binary = root
        .join("target")
        .join("debug")
        .join("dust-fixture-minimal");
    assert!(
        binary.exists(),
        "dust-fixture-minimal binary still missing after cargo build at {binary:?}"
    );
    binary
}

// ── Spawn helpers ─────────────────────────────────────────────────────────────

/// Spawn the minimal fixture and return a `ConformanceRunner` before handshake.
pub async fn spawn_fixture() -> ConformanceRunner {
    let binary = fixture_binary();
    ConformanceRunner::spawn_binary(binary, Duration::from_secs(10))
        .await
        .expect("failed to spawn dust-fixture-minimal")
}

/// Spawn the minimal fixture and complete the ready/host_info handshake.
pub async fn spawn_and_handshake() -> ConformanceRunner {
    let mut runner = spawn_fixture().await;
    runner.do_handshake().await.expect("handshake failed");
    runner
}

/// Spawn the minimal fixture with a custom `DUST_HEARTBEAT_INTERVAL_MS`,
/// without performing the handshake.
///
/// Used by heartbeat conformance tests that need a short interval so they
/// can observe proactive sends and miss-detection within a few seconds.
pub async fn spawn_fixture_with_hb_ms(hb_ms: u64) -> ConformanceRunner {
    let binary = fixture_binary();
    ConformanceRunner::spawn_binary_with_envs(
        binary,
        Duration::from_secs(10),
        &[("DUST_HEARTBEAT_INTERVAL_MS".to_string(), hb_ms.to_string())],
    )
    .await
    .unwrap_or_else(|e| panic!("failed to spawn fixture with HB interval {hb_ms}: {e}"))
}

/// Spawn the minimal fixture with a custom heartbeat interval and complete the
/// ready/host_info handshake.
pub async fn spawn_and_handshake_with_hb_ms(hb_ms: u64) -> ConformanceRunner {
    let mut runner = spawn_fixture_with_hb_ms(hb_ms).await;
    runner
        .do_handshake()
        .await
        .expect("handshake failed in fixture with custom HB interval");
    runner
}

/// Spawn the minimal fixture with a small event ring (for replay-gap tests).
///
/// Sets `DUST_FIXTURE_MAX_RING_EVENTS=<max_events>` so only a few action calls
/// are needed to trigger eviction and produce a `-33007 replay_gap` condition.
pub async fn spawn_fixture_with_small_ring(max_events: usize) -> ConformanceRunner {
    let binary = fixture_binary();
    ConformanceRunner::spawn_binary_with_envs(
        binary,
        Duration::from_secs(10),
        &[(
            "DUST_FIXTURE_MAX_RING_EVENTS".to_string(),
            max_events.to_string(),
        )],
    )
    .await
    .unwrap_or_else(|e| {
        panic!("failed to spawn fixture with ring max_events={max_events}: {e}")
    })
}

/// Spawn the minimal fixture with a small ring and complete the handshake.
pub async fn spawn_and_handshake_with_small_ring(max_events: usize) -> ConformanceRunner {
    let mut runner = spawn_fixture_with_small_ring(max_events).await;
    runner
        .do_handshake()
        .await
        .expect("handshake failed in fixture with small ring");
    runner
}
