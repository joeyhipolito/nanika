#![cfg(unix)]

use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, std::io::Error> {
        let path = std::env::temp_dir().join(format!(
            "observe-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Observer(Child);
impl Drop for Observer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(path: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orchestrator-first-use-pilot"));
    cmd.env("NANIKA_RUST_FIRST_USE_PILOT", "1")
        .args(["observe", "--progress-log"])
        .arg(path)
        .args(["--format", "json"]);
    cmd
}
fn follow(path: &Path) -> Result<(Observer, Receiver<Value>), Box<dyn std::error::Error>> {
    let mut child = command(path)
        .arg("--follow")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("observer stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    Ok((Observer(child), rx))
}
fn wait_refused(observer: &mut Observer) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = observer.0.try_wait()? {
            assert_eq!(status.code(), Some(1));
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("observer did not refuse changed source promptly".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn stage(name: &str) -> String {
    format!(
        "{}\n",
        json!({"schema":"nanika.rust-pilot.progress.v1","kind":"stage","stage":name})
    )
}

#[test]
fn follow_buffers_partial_frames_flushes_appends_and_detaches_without_changing_source() -> TestResult
{
    let scratch = Scratch::new()?;
    let path = scratch.0.join("progress.jsonl");
    let frame = stage("live");
    let split = frame.len() / 2;
    fs::write(&path, &frame[..split])?;
    let (mut observer, rx) = follow(&path)?;
    assert!(matches!(
        rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let mut writer = OpenOptions::new().append(true).open(&path)?;
    writer.write_all(&frame.as_bytes()[split..])?;
    writer.sync_all()?;
    let row = rx.recv_timeout(Duration::from_secs(3))?;
    assert_eq!(row["kind"], "owner_stage");
    assert_eq!(row["source"]["cursor"], frame.len());
    assert!(observer.0.try_wait()?.is_none());
    let before = fs::read(&path)?;
    let modified = fs::metadata(&path)?.modified()?;
    observer.0.kill()?;
    observer.0.wait()?;
    assert_eq!(fs::read(&path)?, before);
    assert_eq!(fs::metadata(&path)?.modified()?, modified);
    writer.write_all(stage("writer-still-active").as_bytes())?;
    assert!(fs::read_to_string(&path)?.contains("writer-still-active"));
    Ok(())
}

#[test]
fn follow_refuses_same_inode_copy_truncate_and_regrow_before_splicing_a_partial_frame() -> TestResult
{
    let scratch = Scratch::new()?;
    let path = scratch.0.join("progress.jsonl");
    let old = format!("{}{{\"type\":", stage("old"));
    fs::write(&path, &old)?;
    let (mut observer, rx) = follow(&path)?;
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3))?["kind"],
        "owner_stage"
    );
    let replacement = format!("{}{}", stage("new-generation"), " ".repeat(old.len() + 128));
    fs::write(&path, replacement)?;
    wait_refused(&mut observer)?;
    assert!(
        rx.try_iter()
            .all(|row| row["body"]["detail"]["stage"] != "new-generation")
    );
    Ok(())
}

#[test]
fn follow_refuses_path_replacement_and_initial_symlink_fifo_inputs() -> TestResult {
    let scratch = Scratch::new()?;
    let path = scratch.0.join("progress.jsonl");
    fs::write(&path, stage("old"))?;
    let (mut observer, rx) = follow(&path)?;
    rx.recv_timeout(Duration::from_secs(3))?;
    fs::rename(&path, scratch.0.join("old"))?;
    fs::write(&path, stage("replacement"))?;
    wait_refused(&mut observer)?;
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink(&path, &link)?;
    assert_eq!(command(&link).output()?.status.code(), Some(1));
    let fifo = scratch.0.join("fifo");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()?
            .success()
    );
    let (mut observer, _) = follow(&fifo)?;
    wait_refused(&mut observer)?;
    Ok(())
}
