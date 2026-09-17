//! Fake-provider acceptance for the bounded experiment runner; no live provider calls.
use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn matched_samples_keep_failures_unknown_metrics_and_source_drift()
-> Result<(), Box<dyn std::error::Error>> {
    let scratch_path = fs::canonicalize(std::env::temp_dir())?
        .join(format!("nanika-experiment-cli-{}", std::process::id()));
    fs::create_dir(&scratch_path)?;
    let scratch = Scratch(scratch_path);
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/experiment/check.py");
    let output = Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(script)
        .arg(scratch.0.join("cases"))
        .arg(env!("CARGO_BIN_EXE_orchestrator-experiment"))
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(records.as_array().map(Vec::len), Some(8));
    assert!(
        records
            .as_array()
            .ok_or("fixture results")?
            .iter()
            .all(|row| row["passed"] == true)
    );
    Ok(())
}
