use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Eq, PartialEq)]
struct Observation {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
    personas: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn create() -> io::Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-cli-differential-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let home = root.join("home");
        let config = root.join("config");
        let personas = root.join("personas");
        let path = root.join("empty-path");
        for directory in [&home, &config, &personas, &path] {
            fs::create_dir_all(directory)?;
        }
        fs::write(home.join("sentinel"), b"unchanged\n")?;
        Ok(Self {
            root,
            home,
            config,
            personas,
            path,
        })
    }

    fn observe(&self, binary: &Path, arguments: &[&str]) -> io::Result<Observation> {
        let output = Command::new(binary)
            .args(arguments)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("ORCHESTRATOR_CONFIG_DIR", &self.config)
            .env("ORCHESTRATOR_PERSONAS_DIR", &self.personas)
            .env("PATH", &self.path)
            .env("TMPDIR", &self.root)
            .output()?;
        Ok(Observation {
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn snapshot(&self) -> io::Result<Vec<(PathBuf, Option<Vec<u8>>)>> {
        snapshot_tree(&self.root, &self.root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn snapshot_tree(root: &Path, directory: &Path) -> io::Result<Vec<(PathBuf, Option<Vec<u8>>)>> {
    let mut entries = Vec::new();
    let mut children = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if child.file_type()?.is_dir() {
            entries.push((relative, None));
            entries.extend(snapshot_tree(root, &path)?);
        } else {
            entries.push((relative, Some(fs::read(path)?)));
        }
    }
    Ok(entries)
}

/// The accepted Go orchestrator binary this differential compares against.
/// This must never resolve to a silent skip: a missing oracle is a hard
/// failure that names exactly what was checked and where, not an
/// `eprintln!` + `Ok(())` green (TRK-1280).
fn accepted_go_binary() -> Result<PathBuf, String> {
    // B5-DESIGN §8.6 rung 1: inside the verification lease the oracle is the
    // one the lease built from the tested commit, and nothing else is
    // consulted — not `ORCHESTRATOR_ACCEPTED_GO_BIN`, and above all not
    // `$HOME/.alluka/bin/orchestrator`, which is the installed binary TRK-1280
    // showed cannot witness a claim about a committed tree.
    if let Some(directory) = std::env::var_os("NANIKA_GO_ORACLE_OUTPUT_DIR") {
        let leased = PathBuf::from(directory).join("orchestrator");
        return if leased.is_file() {
            Ok(leased)
        } else {
            Err(format!(
                "NANIKA_GO_ORACLE_OUTPUT_DIR names no built Go oracle at {}; the verification \
                 lease did not produce one",
                leased.display()
            ))
        };
    }
    if let Some(path) = std::env::var_os("ORCHESTRATOR_ACCEPTED_GO_BIN") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "ORCHESTRATOR_ACCEPTED_GO_BIN={} does not name a file; the accepted Go oracle binary is missing",
                path.display()
            ))
        };
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "HOME is unset and ORCHESTRATOR_ACCEPTED_GO_BIN is not set; cannot locate the accepted Go oracle binary".to_string()
        })?;
    let default_path = home.join(".alluka/bin/orchestrator");
    if default_path.is_file() {
        Ok(default_path)
    } else {
        Err(format!(
            "the accepted Go oracle binary is missing: neither ORCHESTRATOR_ACCEPTED_GO_BIN nor the default {} names an existing file",
            default_path.display()
        ))
    }
}

fn assert_cases_match(cases: &[&[&str]]) -> Result<(), Box<dyn std::error::Error>> {
    let go_binary = accepted_go_binary()?;
    let rust_binary = Path::new(env!("CARGO_BIN_EXE_orchestrator"));
    let fixture = Fixture::create()?;
    let before = fixture.snapshot()?;

    for arguments in cases {
        let go = fixture.observe(&go_binary, arguments)?;
        let rust = fixture.observe(rust_binary, arguments)?;
        assert_eq!(rust, go, "Go/Rust CLI mismatch for {arguments:?}");
    }

    assert_eq!(
        fixture.snapshot()?,
        before,
        "a CLI probe mutated its fixture"
    );
    Ok(())
}

fn assert_audit_cases_match(cases: &[&[&str]]) -> Result<(), Box<dyn std::error::Error>> {
    let go_binary = accepted_go_binary()?;
    let rust_binary = Path::new(env!("CARGO_BIN_EXE_orchestrator"));
    let fixture = Fixture::create()?;
    fs::write(
        fixture.config.join("audits.jsonl"),
        concat!(
            "{\"workspace_id\":\"ws-1\",\"domain\":\"dev\",\"audited_at\":\"2026-07-15T01:02:03Z\",",
            "\"scorecard\":{\"decomposition_quality\":1,\"persona_fit\":1,\"skill_utilization\":1,",
            "\"output_quality\":1,\"rule_compliance\":1,\"overall\":1}}\n",
            "{\"workspace_id\":\"ws-2\",\"domain\":\"work\",\"audited_at\":\"2026-07-16T01:02:03Z\",",
            "\"scorecard\":{\"decomposition_quality\":5,\"persona_fit\":5,\"skill_utilization\":5,",
            "\"output_quality\":5,\"rule_compliance\":5,\"overall\":5}}\n",
        ),
    )?;
    let before = fixture.snapshot()?;

    for arguments in cases {
        let go = fixture.observe(&go_binary, arguments)?;
        let rust = fixture.observe(rust_binary, arguments)?;
        assert_eq!(rust, go, "Go/Rust audit CLI mismatch for {arguments:?}");
    }

    assert_eq!(
        fixture.snapshot()?,
        before,
        "an audit CLI probe mutated its fixture"
    );
    Ok(())
}

#[test]
fn root_and_run_help_match_the_installed_accepted_go_binary()
-> Result<(), Box<dyn std::error::Error>> {
    assert_cases_match(&[
        &[],
        &["--help"],
        &["-h"],
        &["--"],
        &["--", "run", "--help"],
        &["help"],
        &["run", "--help"],
        &["run", "-h"],
        &["help", "run"],
    ])
}

#[test]
fn persistent_flag_placement_and_forms_match_the_installed_accepted_go_binary()
-> Result<(), Box<dyn std::error::Error>> {
    assert_cases_match(&[
        &["--domain", "work"],
        &["--domain", "work", "run", "--help"],
        &["run", "--domain=work", "--help"],
        &["--nanika-dir", "/fixture/nanika", "run", "--help"],
        &["run", "--nanika-dir=/fixture/nanika", "--help"],
        &[
            "--dry-run=true",
            "--max-turns=3",
            "-v=false",
            "run",
            "--help",
        ],
        &[
            "run",
            "--dry-run=false",
            "--max-turns=3",
            "-v=true",
            "--help",
        ],
        &["--dry-run=1", "-v=FALSE", "run", "--help"],
        &["--max-turns=-1", "run", "--help"],
        &["run", "--sequential=T", "--help"],
        &["-vh", "run"],
        &["-vh=false", "run", "--help"],
        &["-hv=false", "run", "--help"],
        &["-vv=false", "run", "--help"],
        &["run", "-vh"],
        &["run", "-hv=false"],
        &["--runtime", "codex", "run", "--help"],
        &["--runtime=codex", "run", "--help"],
        &["--pr=true", "run", "--help"],
        &["--domain", "work", "help", "run"],
        &["help", "--domain", "work", "run"],
        &["help", "run", "--domain", "work"],
    ])
}

#[test]
fn invalid_arguments_exit_codes_and_streams_match_the_installed_accepted_go_binary()
-> Result<(), Box<dyn std::error::Error>> {
    assert_cases_match(&[
        &["--version"],
        &["-V"],
        &["-vV"],
        &["frobnicate"],
        &["contract-summary"],
        &["--wat"],
        &["run", "--wat"],
        &["--wat=x", "run", "--help"],
        &["--wat", "x", "run", "--help"],
        &["--pr", "run", "--help"],
        &["--domain"],
        &["run", "--runtime"],
        &["--dry-run=maybe"],
        &["run", "--pr=maybe"],
        &["run", "-h=maybe"],
        &["run", "-vh=maybe"],
        &["run", "-hv=maybe"],
        &["run", "-vfalse", "-h"],
        &["run", "-vh=false"],
        &["-vh=false", "run"],
        &["--max-turns", "nope", "run", "--help"],
        &["run", "--help", "--wat"],
        &["run", "--wat", "--help"],
        &["run", "--runtime", "--help"],
        &["run"],
        &["run", "--pr", "--no-git", "fixture"],
        &["run", "--codex-review", "fixture"],
        &["run", "--git-isolate=false", "--pr", "fixture"],
    ])
}

#[test]
fn audit_scorecard_outputs_flag_forms_and_unicode_quotes_match_the_accepted_go_binary()
-> Result<(), Box<dyn std::error::Error>> {
    assert_audit_cases_match(&[
        &["audit", "scorecard"],
        &["--last=0x1", "audit", "scorecard", "--format=json"],
        &["audit", "--last=02", "scorecard", "--format", "json"],
        &["audit", "scorecard", "--last", "+1", "--format", "json"],
        &["audit", "scorecard", "--last=2_0", "--format=json"],
        &["audit", "scorecard", "--last=0x_1", "--format=json"],
        &["audit", "scorecard", "--last=0b_1", "--format=json"],
        &["audit", "scorecard", "--last=0o_1", "--format=json"],
        &["audit", "scorecard", "--last=00_1", "--format=json"],
        &["audit", "scorecard", "--last=-1", "--format=JSON"],
        &["-v", "audit", "-v", "scorecard", "ignored", "--format=json"],
        &[
            "--domain",
            "work",
            "audit",
            "scorecard",
            "--domain",
            "dev",
            "--format=json",
        ],
        &["-", "audit", "-", "scorecard", "", "--format=json"],
        &["audit", "scorecard", "--", "--format=json", "--last=1"],
        &["audit", "scorecard", "--domain", "\u{e000}"],
        &["audit", "scorecard", "--domain", "\u{0600}"],
        &["audit", "scorecard", "--domain", "\u{0378}"],
        &["audit", "scorecard", "--domain", "\u{fdd0}"],
        &["audit", "scorecard", "--domain", "\u{fff9}"],
        &["audit", "scorecard", "--domain", "\u{1bca0}"],
        &["audit", "scorecard", "--domain", "\u{e0001}"],
        &["audit", "scorecard", "--domain", "\u{f0000}"],
    ])
}

#[test]
fn audit_scorecard_empty_store_bytes_match_the_accepted_go_binary()
-> Result<(), Box<dyn std::error::Error>> {
    assert_cases_match(&[
        &["audit", "scorecard"],
        &["audit", "scorecard", "--format=json"],
        &["audit", "scorecard", "--domain", "dev", "--last=0x2"],
    ])
}

#[test]
fn audit_scorecard_invalid_go_int_forms_match_go_rejection_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let go_binary = accepted_go_binary()?;
    let rust_binary = Path::new(env!("CARGO_BIN_EXE_orchestrator"));
    let fixture = Fixture::create()?;
    let before = fixture.snapshot()?;
    for arguments in [
        &["audit", "scorecard", "--last=08"][..],
        &["audit", "scorecard", "--last=0x"][..],
        &["audit", "scorecard", "--last=2__0"][..],
        &["audit", "scorecard", "--last=9223372036854775808"][..],
        &["audit", "scorecard", "--last=-9223372036854775809"][..],
    ] {
        let go = fixture.observe(&go_binary, arguments)?;
        let rust = fixture.observe(rust_binary, arguments)?;
        assert_eq!(
            rust.exit_code, go.exit_code,
            "Go/Rust audit rejection mismatch for {arguments:?}"
        );
        assert_ne!(rust.exit_code, Some(0), "{arguments:?}");
        assert!(rust.stdout.is_empty(), "{arguments:?}");
    }
    assert_eq!(fixture.snapshot()?, before);
    Ok(())
}
