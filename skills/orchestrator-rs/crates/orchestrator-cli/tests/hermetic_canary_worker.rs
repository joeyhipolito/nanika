#![cfg(all(unix, feature = "verification-process-canary"))]

use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static CASE: AtomicU64 = AtomicU64::new(1);

#[test]
fn legacy_environment_and_cwd_token_cannot_authorize_a_worker_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "orchestrator-cli-canary-worker-legacy-{}-{}",
        std::process::id(),
        CASE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root)?;
    let token = root.join(".canary-launch-token");
    let sentinel = root.join("sentinel.txt");
    fs::write(&token, b"attacker-token\n")?;

    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .arg("--hermetic-canary-worker")
        .current_dir(&root)
        .env("NANIKA_CANARY_LAUNCH_TOKEN", "attacker-token")
        .env("NANIKA_CANARY_SENTINEL_PATH", &sentinel)
        .stdin(Stdio::null())
        .output()?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&token)?, b"attacker-token\n");
    assert!(!sentinel.exists());
    let names = fs::read_dir(&root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        names,
        vec![std::ffi::OsString::from(".canary-launch-token")]
    );

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn valid_challenge_emits_proof_but_cannot_mutate_attacker_selected_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "orchestrator-cli-canary-worker-valid-{}-{}",
        std::process::id(),
        CASE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root)?;
    let token = root.join(".canary-launch-token");
    let sentinel = root.join("sentinel.txt");
    fs::write(&token, b"attacker-token\n")?;
    let mut challenge = b"nanika-hermetic-canary-challenge-v1\0".to_vec();
    challenge.extend_from_slice(&[0x5a; 32]);

    let mut child = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .arg("--hermetic-canary-worker")
        .current_dir(&root)
        .env("NANIKA_CANARY_LAUNCH_TOKEN", "attacker-token")
        .env("NANIKA_CANARY_SENTINEL_PATH", &sentinel)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("worker stdin was not piped")?
        .write_all(&challenge)?;
    let output = child.wait_with_output()?;

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(
        output
            .stdout
            .starts_with(b"nanika-hermetic-canary-proof-v1\n")
    );
    assert_eq!(fs::read(&token)?, b"attacker-token\n");
    assert!(!sentinel.exists());
    let names = fs::read_dir(&root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        names,
        vec![std::ffi::OsString::from(".canary-launch-token")]
    );

    fs::remove_dir_all(root)?;
    Ok(())
}
