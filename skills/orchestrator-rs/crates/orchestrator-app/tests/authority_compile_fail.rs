//! Hermetic downstream compile-contract proofs.
//!
//! The source package and lockfile are committed below `tests/fixtures`; this
//! harness never generates or mutates downstream source. Every probe executes
//! `cargo check --locked --offline` with an external target directory.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

static CASE: AtomicU64 = AtomicU64::new(1);

const CAPTURED_RUSTC: &str = env!("NANIKA_CAPTURED_RUSTC");
const AUTHORITY_ORIGIN_SCHEMA: &str = "nanika-authority-downstream-lock-origin-v1";
const AUTHORITY_ORIGIN_SOURCE: &str = "skills/orchestrator-rs/Cargo.lock";
const AUTHORITY_ORIGIN_BASELINE: &str = "skills/orchestrator-rs/tests/rust-lock-baseline.json";
const AUTHORITY_ORIGIN_DERIVATION: &str = "transitive package closure of orchestrator-app@0.1.0 and cap-std@4.0.2 plus the fixture root package; original package records preserved byte-for-byte";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityLockOrigin {
    schema: String,
    source: String,
    source_sha256: String,
    source_baseline: String,
    derivation: String,
    cargo_lock_sha256: String,
}

struct DownstreamFixture {
    root: PathBuf,
    target: PathBuf,
    remove_target_on_drop: bool,
    manifest_bytes: Vec<u8>,
    lockfile_bytes: Vec<u8>,
}

impl DownstreamFixture {
    fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/authority-downstream");
        validate_fixture_tree(&root)?;
        let (target, remove_target_on_drop) = compile_target()?;
        let fixture = Self {
            manifest_bytes: fs::read(root.join("Cargo.toml"))?,
            lockfile_bytes: fs::read(root.join("Cargo.lock"))?,
            root,
            target,
            remove_target_on_drop,
        };
        fixture.validate_inputs()?;
        Ok(fixture)
    }

    fn rejects(
        &self,
        name: &str,
        diagnostic_fragments: &[&str],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = self.check(name)?;
        if output.status.success() {
            return Err(std::io::Error::other(format!(
                "downstream authority probe {name} unexpectedly compiled"
            ))
            .into());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !diagnostic_fragments
            .iter()
            .all(|fragment| stderr.contains(fragment))
        {
            return Err(std::io::Error::other(format!(
                "downstream authority probe {name} failed unexpectedly: {stderr}"
            ))
            .into());
        }
        Ok(())
    }

    fn accepts(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let output = self.check(name)?;
        if output.status.success() {
            return Ok(());
        }
        Err(std::io::Error::other(format!(
            "valid downstream authority probe {name} did not compile: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
        .into())
    }

    fn check(&self, name: &str) -> Result<Output, Box<dyn std::error::Error>> {
        self.validate_inputs()?;
        let mut command = cargo_command(&self.target)?;
        let output = command
            .arg("check")
            .arg("--manifest-path")
            .arg(self.root.join("Cargo.toml"))
            .arg("--bin")
            .arg(name)
            .arg("--locked")
            .arg("--offline")
            .arg("--quiet")
            .output()?;
        self.validate_inputs()?;
        Ok(output)
    }

    fn validate_inputs(&self) -> Result<(), Box<dyn std::error::Error>> {
        validate_fixture_tree(&self.root)?;
        if fs::read(self.root.join("Cargo.toml"))? != self.manifest_bytes
            || fs::read(self.root.join("Cargo.lock"))? != self.lockfile_bytes
        {
            return Err(std::io::Error::other(
                "authority fixture manifest or lockfile changed during verification",
            )
            .into());
        }
        Ok(())
    }
}

impl Drop for DownstreamFixture {
    fn drop(&mut self) {
        if self.remove_target_on_drop {
            let _ = fs::remove_dir_all(&self.target);
        }
    }
}

fn compile_target() -> Result<(PathBuf, bool), Box<dyn std::error::Error>> {
    if let Some(target) = env::var_os("NANIKA_AUTHORITY_COMPILE_TARGET_DIR") {
        let target = absolute_path("NANIKA_AUTHORITY_COMPILE_TARGET_DIR", PathBuf::from(target))?;
        let cache_root = required_environment_path("NANIKA_AUTHORITY_COMPILE_CACHE_ROOT")?;
        let workspace_id = required_digest("NANIKA_AUTHORITY_COMPILE_WORKSPACE_ID")?;
        let fixture_digest = required_digest("NANIKA_AUTHORITY_FIXTURE_SHA256")?;
        if fixture_digest == workspace_id {
            return Err(std::io::Error::other(
                "authority fixture and workspace identities must be independently bound",
            )
            .into());
        }
        let expected_parent = cache_root
            .join("cargo-target/workspaces")
            .join(&workspace_id);
        if !target.starts_with(expected_parent) {
            return Err(std::io::Error::other(
                "authority target is not bound to the leased physical workspace",
            )
            .into());
        }
        validate_private_cache_tree(&cache_root, &target, "authority target")?;
        return Ok((target, false));
    }

    let case = CASE.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let target = env::temp_dir().join(format!(
        "orchestrator-authority-target-{}-{case}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&target)?;
    set_private_directory_mode(&target)?;
    Ok((target, true))
}

fn cargo_command(target: &Path) -> Result<Command, Box<dyn std::error::Error>> {
    let cargo = required_trusted_executable("CARGO")?;
    let rustc = resolve_trusted_rustc(
        env::var_os("RUSTC"),
        Path::new(CAPTURED_RUSTC),
        verified_authority_environment(),
    )?;
    let mut command = Command::new(cargo);
    command
        .env_clear()
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("CARGO_HOME", required_environment_path("CARGO_HOME")?)
        .env("RUSTUP_HOME", required_environment_path("RUSTUP_HOME")?)
        .env("HOME", required_environment_path("HOME")?)
        .env("RUSTC", rustc)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_NET_OFFLINE", "true");
    for variable in [
        "NANIKA_SHELL_ENV_CLEAN",
        "NANIKA_CARGO_LEASE_LOCK",
        "NANIKA_CARGO_LEASE_WORKSPACE_ID",
        "NANIKA_CARGO_LEASE_TOOL",
        "NANIKA_CARGO_LEASE_GUARD_PID",
        "NANIKA_CARGO_LEASE_FD",
        "NANIKA_CARGO_LEASE_ID",
        "NANIKA_CARGO_LEASE_OPERATION",
        "NANIKA_CARGO_LEASE_NONCE",
        "NANIKA_VERIFICATION_LEASE_WORKSPACE",
        "NANIKA_TESTED_TREE_SHA256",
        "NANIKA_AUTHORITY_FIXTURE_SHA256",
        "NANIKA_CARGO_CONFIG_SHA256",
        "NANIKA_CARGO_ENVIRONMENT_SCHEMA",
    ] {
        if let Ok(value) = env::var(variable) {
            if value.is_empty() || value.contains('\n') {
                return Err(std::io::Error::other(format!(
                    "{variable} must be nonempty and single-line"
                ))
                .into());
            }
            command.env(variable, value);
        }
    }
    Ok(command)
}

fn validate_fixture_tree(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let expected = [
        "Cargo.lock",
        "Cargo.toml",
        "ORIGIN.json",
        "src",
        "src/bin",
        "src/bin/authorize_without_enrollment.rs",
        "src/bin/bound_process_preflight_not_clone_copy.rs",
        "src/bin/clone_raw_production_directory.rs",
        "src/bin/concurrent_raw_projection_bypass.rs",
        "src/bin/forge_bound_process_preflight.rs",
        "src/bin/forge_process_preflight.rs",
        "src/bin/forge_root.rs",
        "src/bin/forge_untyped_effect_row.rs",
        "src/bin/mint_enrollment.rs",
        "src/bin/mint_legacy_quiescence.rs",
        "src/bin/mint_process_start_identity.rs",
        "src/bin/mint_projection_receipt.rs",
        "src/bin/mint_retry_authorization.rs",
        "src/bin/mint_storage_actor.rs",
        "src/bin/name_durable_process_actor.rs",
        "src/bin/name_enrolled_provider_launch.rs",
        "src/bin/name_prepared_provider_launch.rs",
        "src/bin/process_preflight_not_clone_copy.rs",
        "src/bin/promote_fixture.rs",
        "src/bin/raw_fixture_root_cannot_authorize.rs",
        "src/bin/unbound_process_effect_row.rs",
        "src/bin/valid_usage.rs",
    ];
    let mut observed = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(std::io::Error::other("authority fixture contains a symlink").into());
            }
            let relative = path.strip_prefix(root)?.to_string_lossy().into_owned();
            observed.push(relative);
            if metadata.is_dir() {
                pending.push(path);
            } else if !metadata.is_file() {
                return Err(
                    std::io::Error::other("authority fixture contains a special file").into(),
                );
            }
        }
    }
    observed.sort();
    if observed != expected {
        return Err(std::io::Error::other(format!(
            "authority fixture has unexpected entries: {observed:?}"
        ))
        .into());
    }
    let origin: AuthorityLockOrigin =
        serde_json::from_str(&fs::read_to_string(root.join("ORIGIN.json"))?)?;
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_lock_sha256 = sha256_hex(&fs::read(workspace_root.join("Cargo.lock"))?);
    let downstream_lock_sha256 = sha256_hex(&fs::read(root.join("Cargo.lock"))?);
    if origin.schema != AUTHORITY_ORIGIN_SCHEMA
        || origin.source != AUTHORITY_ORIGIN_SOURCE
        || origin.source_baseline != AUTHORITY_ORIGIN_BASELINE
        || origin.derivation != AUTHORITY_ORIGIN_DERIVATION
        || origin.source_sha256 != source_lock_sha256
        || origin.cargo_lock_sha256 != downstream_lock_sha256
    {
        return Err(std::io::Error::other("authority lock origin is not truthful").into());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn required_digest(variable: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = env::var(variable)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(std::io::Error::other(format!(
            "{variable} must be a lowercase SHA-256 digest"
        ))
        .into());
    }
    Ok(value)
}

fn absolute_path(variable: &str, path: PathBuf) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if path.is_absolute() && !path.as_os_str().is_empty() {
        return Ok(path);
    }
    Err(std::io::Error::other(format!("{variable} must be absolute")).into())
}

fn required_environment_path(variable: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let value = env::var_os(variable)
        .ok_or_else(|| std::io::Error::other(format!("{variable} is required")))?;
    absolute_path(variable, PathBuf::from(value))
}

fn required_trusted_executable(variable: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = required_environment_path(variable)?;
    validate_trusted_executable(path, variable)
}

fn resolve_trusted_rustc(
    explicit: Option<std::ffi::OsString>,
    captured: &Path,
    verified: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = match explicit {
        Some(path) => PathBuf::from(path),
        None if verified => {
            return Err(std::io::Error::other(
                "RUSTC is required for verified authority compilation",
            )
            .into());
        }
        None => captured.to_path_buf(),
    };
    validate_trusted_executable(path, "RUSTC")
}

fn verified_authority_environment() -> bool {
    env::var_os("NANIKA_SHELL_ENV_CLEAN").is_some()
        || env::var_os("NANIKA_AUTHORITY_COMPILE_TARGET_DIR").is_some()
}

fn validate_trusted_executable(
    path: PathBuf,
    label: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = absolute_path(label, path)?;
    if fs::canonicalize(&path)? != path {
        return Err(
            std::io::Error::other(format!("{label} must identify a canonical executable")).into(),
        );
    }
    validate_private_regular_file(&path, label)?;
    #[cfg(unix)]
    if fs::symlink_metadata(&path)?.mode() & 0o111 == 0 {
        return Err(std::io::Error::other(format!("{label} is not executable")).into());
    }
    Ok(path)
}

#[cfg(unix)]
fn validate_private_regular_file(
    path: &Path,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let metadata = fs::symlink_metadata(path)?;
    let effective_uid = rustix::process::geteuid().as_raw();
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || (metadata.uid() != 0 && metadata.uid() != effective_uid)
        || metadata.mode() & 0o022 != 0
    {
        return Err(
            std::io::Error::other(format!("{label} has an unsafe type, owner, or mode")).into(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_regular_file(
    _path: &Path,
    _label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    Err(std::io::Error::other("authority compile contracts require Unix").into())
}

#[cfg(unix)]
fn set_private_directory_mode(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_mode(_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(unix)]
fn validate_private_cache_tree(
    cache_root: &Path,
    path: &Path,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_root = fs::canonicalize(cache_root)?;
    let canonical_path = fs::canonicalize(path)?;
    if canonical_root != cache_root || canonical_path != path || !path.starts_with(cache_root) {
        return Err(std::io::Error::other(format!(
            "{label} contains a symlinked or escaping component"
        ))
        .into());
    }
    let effective_uid = rustix::process::geteuid().as_raw();
    let mut current = path;
    loop {
        let metadata = fs::symlink_metadata(current)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != effective_uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(std::io::Error::other(format!(
                "{label} has an unsafe private component: {}",
                current.display()
            ))
            .into());
        }
        if current == cache_root {
            break;
        }
        current = current
            .parent()
            .ok_or_else(|| std::io::Error::other(format!("{label} escaped cache root")))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_cache_tree(
    _cache_root: &Path,
    _path: &Path,
    _label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    Err(std::io::Error::other("persistent authority target requires Unix").into())
}

#[test]
fn downstream_cannot_forge_or_cross_authority_states() -> Result<(), Box<dyn std::error::Error>> {
    let downstream = DownstreamFixture::open()?;
    downstream.accepts("valid_usage")?;
    downstream.rejects(
        "concurrent_raw_projection_bypass",
        &[
            "unresolved imports",
            "replace_checkpoint",
            "replace_fixture_event",
        ],
    )?;
    downstream.rejects("forge_root", &["unresolved import", "CapabilityRoot"])?;
    downstream.rejects(
        "clone_raw_production_directory",
        &["no method named `directory`", "ProductionBoundary"],
    )?;
    downstream.rejects("mint_enrollment", &["field `_private`", "private"])?;
    downstream.rejects(
        "mint_process_start_identity",
        &[
            "cannot find value `ProcessStartIdentity`",
            "orchestrator_app",
        ],
    )?;
    downstream.rejects(
        "mint_storage_actor",
        &[
            "associated function `new` is private",
            "StorageActorAuthority",
        ],
    )?;
    downstream.rejects(
        "mint_projection_receipt",
        &[
            "associated function `compatibility` is private",
            "ProjectionReceipt",
        ],
    )?;
    downstream.rejects(
        "mint_retry_authorization",
        &[
            "associated function `policy` is private",
            "associated function `operator` is private",
            "RetryAuthorization",
        ],
    )?;
    downstream.rejects(
        "mint_legacy_quiescence",
        &[
            "associated function `inspect` is private",
            "cannot construct `LegacyQuiescenceProof`",
            "no method named `clone`",
            "use of moved value: `proof`",
            "private",
        ],
    )?;
    downstream.rejects(
        "promote_fixture",
        &[
            "expected `&AuthorizedProductionRuntimeHome`",
            "found reference `&AuthorizedFixtureRuntimeHome`",
        ],
    )?;
    downstream.rejects(
        "authorize_without_enrollment",
        &["this method takes 1 argument", "argument #1"],
    )?;
    downstream.rejects(
        "raw_fixture_root_cannot_authorize",
        &[
            "expected `&FreshFixtureAuthority`",
            "found reference `&IsolatedFixtureRoot`",
        ],
    )?;
    downstream.rejects(
        "forge_process_preflight",
        &[
            "ProcessPreflight",
            "reason",
            "request_fingerprint",
            "service_identity",
            "_service_borrow",
            "private",
        ],
    )?;
    downstream.rejects(
        "forge_bound_process_preflight",
        &[
            "BoundProcessPreflight",
            "reason",
            "request_fingerprint",
            "private",
        ],
    )?;
    downstream.rejects(
        "process_preflight_not_clone_copy",
        &["ProcessPreflight", "Clone", "Copy", "trait bound"],
    )?;
    downstream.rejects(
        "bound_process_preflight_not_clone_copy",
        &["BoundProcessPreflight", "Clone", "Copy", "trait bound"],
    )?;
    downstream.rejects(
        "name_prepared_provider_launch",
        &[
            "cannot find type `PreparedProviderLaunch` in crate `orchestrator_app`",
            "module `durable_process_service` is private",
        ],
    )?;
    downstream.rejects(
        "name_enrolled_provider_launch",
        &[
            "cannot find type `EnrolledProviderLaunch` in crate `orchestrator_app`",
            "module `durable_process_service` is private",
        ],
    )?;
    downstream.rejects(
        "name_durable_process_actor",
        &[
            "cannot find type `DurableProcessActor` in crate `orchestrator_app`",
            "module `durable_process_service` is private",
        ],
    )?;
    // CF-M3-W9. The untyped effect-row constructor is unreachable from
    // outside the crate, and the one that is reachable cannot be called
    // without the process request that binds the row to a gated spawn.
    downstream.rejects(
        "forge_untyped_effect_row",
        &[
            "associated function `for_mission` is private",
            "OutboxIntent",
        ],
    )?;
    downstream.rejects(
        "unbound_process_effect_row",
        &[
            "this function takes 7 arguments but 6 arguments were supplied",
            "for_process",
        ],
    )?;
    Ok(())
}

#[test]
fn ordinary_rustc_selection_uses_the_cargo_captured_compiler()
-> Result<(), Box<dyn std::error::Error>> {
    let selected = resolve_trusted_rustc(None, Path::new(CAPTURED_RUSTC), false)?;
    assert_eq!(selected, Path::new(CAPTURED_RUSTC));
    Ok(())
}

#[test]
fn verified_rustc_selection_requires_an_explicit_compiler() -> Result<(), Box<dyn std::error::Error>>
{
    let Err(error) = resolve_trusted_rustc(None, Path::new(CAPTURED_RUSTC), true) else {
        return Err(std::io::Error::other(
            "verified selection unexpectedly used the captured fallback",
        )
        .into());
    };
    assert!(
        error
            .to_string()
            .contains("RUSTC is required for verified authority compilation")
    );
    Ok(())
}

#[test]
fn invalid_explicit_rustc_never_falls_back_to_the_captured_compiler()
-> Result<(), Box<dyn std::error::Error>> {
    let Err(error) = resolve_trusted_rustc(
        Some(std::ffi::OsString::from("relative-rustc")),
        Path::new(CAPTURED_RUSTC),
        false,
    ) else {
        return Err(std::io::Error::other("an invalid explicit RUSTC was accepted").into());
    };
    assert!(error.to_string().contains("RUSTC must be absolute"));
    Ok(())
}

#[test]
fn valid_explicit_rustc_precedes_an_invalid_captured_compiler()
-> Result<(), Box<dyn std::error::Error>> {
    let selected = resolve_trusted_rustc(
        Some(std::ffi::OsString::from(CAPTURED_RUSTC)),
        Path::new("not-an-absolute-compiler"),
        false,
    )?;
    assert_eq!(selected, Path::new(CAPTURED_RUSTC));
    Ok(())
}
