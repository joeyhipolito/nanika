use serde_json::Value;
use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "nanika-portal-cli-{}-{}",
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

fn fixture(scratch: &Scratch, mode: &str, enabled: bool) -> Result<(Output, PathBuf)> {
    let repo = scratch.0.join("repo");
    fs::create_dir(&repo)?;
    fs::write(repo.join("note.txt"), "before\n")?;
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "add",
            ".",
        ],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "input",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()?
                .success()
        );
    }
    let prompt = scratch.0.join("prompt.md");
    fs::write(&prompt, "Make the requested fixture change.")?;
    let script = r#"#!/usr/bin/python3
import json,os,sys,subprocess,shlex,pathlib
if '--version' in sys.argv:
 print('codex-cli 0.154.0');sys.exit()
prompt=sys.stdin.read()
mode=@MODE@
helper=@HELPER@
enabled=@ENABLED@
if enabled:
 assert '--add-dir' in sys.argv and 'Portal output cap is ON' in prompt
 logs=pathlib.Path(sys.argv[sys.argv.index('--add-dir')+1]);assert logs.name=='portal-logs'
else:
 assert '--add-dir' not in sys.argv and 'Portal output cap is ON' not in prompt
 logs=None
def emit(row):print(json.dumps(row),flush=True)
emit({'type':'thread.started','thread_id':'portal-fixture'})
emit({'type':'turn.started'})
count=0 if mode=='no-shell' else 2 if mode=='tamper-prior' else 1
for n in range(count):
 program="printf 'error: preserved failure evidence\\n'; python3 -c 'print(\"noise\"*20000)'; printf 'after\\n' > note.txt"
 if mode=='nonzero':program+='; exit 7'
 if n==1:
  first=logs/'command-0.log'
  program='python3 -c '+shlex.quote('from pathlib import Path;p=Path('+repr(str(first))+');b=p.read_bytes();p.write_bytes(b.replace(b"error",b"xxxxx",1))')
 if enabled and mode!='unwrapped':
  log=logs/('command-'+str(n)+'.log')
  argv=[helper,'--portal','on','--output-cap','on','--log',str(log),'--','/bin/sh','-c',program]
 else:argv=['/bin/sh','-c',program]
 display=shlex.join(['/bin/sh','-c',shlex.join(argv)])
 emit({'type':'item.started','item':{'id':'cmd-'+str(n),'type':'command_execution','command':display,'aggregated_output':'','exit_code':None,'status':'in_progress'}})
 result=subprocess.run(argv,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
 output=result.stdout
 if mode=='truncated':output=output[:40]
 if mode=='bad-exit':code=result.returncode+1
 else:code=result.returncode
 emit({'type':'item.completed','item':{'id':'cmd-'+str(n),'type':'command_execution','command':display,'aggregated_output':output,'exit_code':code,'status':'completed' if code==0 else 'failed'}})
 if mode=='symlink':
  log.unlink();log.symlink_to(pathlib.Path.cwd()/'note.txt')
 if mode=='extra':(logs/'extra.txt').write_text('extra')
emit({'type':'item.completed','item':{'id':'answer','type':'agent_message','text':'Fixture complete.'}})
emit({'type':'turn.completed','usage':{'input_tokens':100,'cached_input_tokens':50,'cache_write_input_tokens':0,'output_tokens':12,'reasoning_output_tokens':0}})
"#;
    let script = script
        .replace("@MODE@", &serde_json::to_string(mode)?)
        .replace(
            "@HELPER@",
            &serde_json::to_string(&fs::canonicalize(env!(
                "CARGO_BIN_EXE_orchestrator-output"
            ))?)?,
        )
        .replace("@ENABLED@", if enabled { "True" } else { "False" });
    let provider = scratch.0.join("codex");
    fs::write(&provider, script)?;
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700))?;
    let out = scratch.0.join("out");
    let mut command = Command::new(env!("CARGO_BIN_EXE_orchestrator-first-use-pilot"));
    command
        .env("NANIKA_RUST_FIRST_USE_PILOT", "1")
        .args(["code", "--runtime", "codex", "--repo"])
        .arg(&repo)
        .arg("--prompt-file")
        .arg(&prompt)
        .arg("--output-dir")
        .arg(&out)
        .arg("--codex")
        .arg(&provider)
        .args([
            "--timeout-secs",
            "30",
            "--feature",
            if enabled {
                "portal-output-cap=on"
            } else {
                "portal-output-cap=off"
            },
        ]);
    let output = command.output()?;
    assert_eq!(fs::read_to_string(repo.join("note.txt"))?, "before\n");
    Ok((output, out))
}
fn read(path: &Path, name: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path.join(name))?)?)
}

#[test]
fn capped_worker_commands_keep_full_hash_bound_logs_and_real_exit_status() -> Result {
    for mode in ["success", "nonzero"] {
        let scratch = Scratch::new()?;
        let (result, out) = fixture(&scratch, mode, true)?;
        assert!(
            result.status.success(),
            "{mode}: {}",
            read(&out, "pilot-result.json")?
        );
        let report = read(&out, "portal-application.json")?;
        assert_eq!(report["applied"], true);
        assert_eq!(report["verified_wrapped_calls"], 1);
        assert!(report["full_log_bytes"].as_u64().ok_or("bytes")? > 100000);
        assert!(report["returned_bytes"].as_u64().ok_or("returned")? <= 16384);
        assert_eq!(
            report["commands"][0]["command_exit_code"],
            if mode == "nonzero" { 7 } else { 0 }
        );
        let full = fs::read(out.join("portal-logs/command-0.log"))?;
        assert!(full.starts_with(b"error: preserved failure evidence\n"));
        let usage = read(&out, "worker-usage.json")?;
        assert_eq!(usage["portal_requested"], "on");
        assert_eq!(usage["portal_effective"], "on");
        let configured = read(&out, "run-features.json")?;
        assert!(configured["entries"][3]["applied"].is_null());
    }
    Ok(())
}
#[test]
fn off_bypasses_helper_and_preserves_raw_worker_output() -> Result {
    let scratch = Scratch::new()?;
    let (result, out) = fixture(&scratch, "success", false)?;
    assert!(
        result.status.success(),
        "{}",
        read(&out, "pilot-result.json")?
    );
    assert!(!out.join("portal-logs").exists());
    assert!(!out.join("portal-application.json").exists());
    let stdout = fs::read_to_string(out.join("codex-stdout.jsonl"))?;
    assert!(stdout.len() > 100000);
    assert_eq!(read(&out, "worker-usage.json")?["portal_effective"], "off");
    Ok(())
}
#[test]
fn no_shell_calls_are_not_reported_as_applied() -> Result {
    let scratch = Scratch::new()?;
    let (result, out) = fixture(&scratch, "no-shell", true)?;
    assert!(
        result.status.success(),
        "{}",
        read(&out, "pilot-result.json")?
    );
    let report = read(&out, "portal-application.json")?;
    assert_eq!(report["applied"], false);
    assert_eq!(report["status"], "no-shell-calls");
    assert!(read(&out, "worker-usage.json")?["portal_effective"].is_null());
    Ok(())
}
#[test]
fn bypass_truncation_tampering_symlinks_and_mismatched_exits_fail_validation() -> Result {
    for (mode, expected_reason) in [
        ("unwrapped", "unexpected argument shape"),
        ("truncated", "response is incomplete or invalid"),
        ("tamper-prior", "full log changed"),
        ("symlink", "symbolic"),
        ("extra", "unexpected artifacts"),
        ("bad-exit", "receipt, and command do not agree"),
    ] {
        let scratch = Scratch::new()?;
        let (result, out) = fixture(&scratch, mode, true)?;
        assert_eq!(
            result.status.code(),
            Some(1),
            "{mode}: {}",
            read(&out, "pilot-result.json")?
        );
        let report = read(&out, "portal-application.json")?;
        assert_eq!(report["applied"], false, "{mode}");
        assert_eq!(report["status"], "failed-validation", "{mode}");
        let reason = report["reason"].as_str().ok_or("validation reason")?;
        assert!(reason.contains(expected_reason), "{mode}: {reason}");
        assert!(!out.join("answer.md").exists());
        assert_eq!(
            read(&out, "pilot-result.json")?["provider_completed"],
            false
        );
        assert!(read(&out, "worker-usage.json")?["portal_effective"].is_null());
    }
    Ok(())
}
