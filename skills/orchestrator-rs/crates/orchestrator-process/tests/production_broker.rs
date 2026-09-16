#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use orchestrator_process::{
    AuthorizedProcessError, AuthorizedProcessErrorClassification, AuthorizedProcessOutcome,
    CancellationToken, ExecutableFileAttestation, MAX_ATTESTED_EXECUTABLE_BYTES, ProcessError,
    ProcessNotStartedReason, ProcessOwnedDirectory, ProcessOwnedDirectoryKind, ProcessReport,
    ProcessRequestBinding, ProcessSpec, ProcessStartGate, ProcessSupervisor, ProcessTermination,
    ProductionProcessLaunchAuthority, RecordedProcessIdentityStatus,
    inspect_recorded_process_identity, recover_orphaned_process_directories,
};
use rustix::process::{Pid, Signal, kill_process_group};
use sha2::{Digest, Sha256};

const BROKER_BINARY: &str = env!("CARGO_BIN_EXE_orchestrator-process-broker");
static CASE_NONCE: AtomicU64 = AtomicU64::new(1);
static CASE_SERIALIZATION: Mutex<()> = Mutex::new(());

fn test_request_binding() -> ProcessRequestBinding {
    ProcessRequestBinding::from_bytes([0x5a; 32])
}

fn started(outcome: AuthorizedProcessOutcome) -> Result<ProcessReport, std::io::Error> {
    match outcome {
        AuthorizedProcessOutcome::Started(report) => Ok(report),
        AuthorizedProcessOutcome::NotStarted(receipt) => Err(std::io::Error::other(format!(
            "production target was not released: NotStarted({receipt:?})"
        ))),
        AuthorizedProcessOutcome::Uncertain(receipt) => Err(std::io::Error::other(format!(
            "production target release was uncertain: {receipt:?}"
        ))),
    }
}

#[derive(Clone, Copy)]
enum TestGateDecision {
    ReleaseAuthorized,
    Rejected,
    Indeterminate,
}

fn run_with_gate_decision(
    supervisor: &ProcessSupervisor,
    spec: &ProcessSpec,
    authority: &ProductionProcessLaunchAuthority,
    decision: TestGateDecision,
) -> Result<AuthorizedProcessOutcome, Box<dyn std::error::Error>> {
    let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
    let actor = thread::spawn(move || {
        let Ok((request, started_authority)) = gate_authority.receive() else {
            return Ok(());
        };
        match decision {
            TestGateDecision::ReleaseAuthorized => {
                request.release_authorized()?;
                let started = started_authority.receive_started()?;
                assert!(started.receipt().matches_request(&[0x5a; 32]));
                started.persisted()
            }
            TestGateDecision::Rejected => request.reject(),
            TestGateDecision::Indeterminate => request.indeterminate(),
        }
    });
    let outcome = supervisor.run_authorized(spec, authority, &mut gate);
    drop(gate);
    actor
        .join()
        .map_err(|_| std::io::Error::other("gate actor panicked"))??;
    Ok(outcome?)
}

struct Case {
    path: PathBuf,
    // Disposable production authorities are a sequential-canary surface. Keep
    // the 64 MiB streaming proof from competing with five-second gate tests for
    // CPU and fsync progress on a loaded host.
    _serialization: MutexGuard<'static, ()>,
}

impl Case {
    fn create(label: &str) -> std::io::Result<Self> {
        let serialization = CASE_SERIALIZATION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let nonce = CASE_NONCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "orchestrator-production-broker-{}-{nonce}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("root/bin"))?;
        std::fs::create_dir_all(path.join("root/workspaces/admitted"))?;
        std::fs::create_dir(path.join("outside"))?;
        Ok(Self {
            path,
            _serialization: serialization,
        })
    }

    fn root(&self) -> PathBuf {
        self.path.join("root")
    }

    fn admitted_cwd(&self) -> PathBuf {
        self.root().join("workspaces/admitted")
    }

    fn make_root_private(&self) -> std::io::Result<()> {
        std::fs::set_permissions(self.root(), std::fs::Permissions::from_mode(0o700))
    }

    fn ownership_journal(&self) -> PathBuf {
        self.path.join("process-ownership")
    }

    fn persist_owned_directories(&self, records: &[ProcessOwnedDirectory]) -> std::io::Result<()> {
        let journal = self.ownership_journal();
        std::fs::create_dir(&journal)?;
        for (index, record) in records.iter().enumerate() {
            let path = journal.join(format!("record-{index}"));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            file.write_all(&record.encode())?;
            file.sync_all()?;
        }
        let mut count = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(journal.join("count"))?;
        write!(count, "{}", records.len())?;
        count.sync_all()?;
        File::open(journal)?.sync_all()?;
        File::open(&self.path)?.sync_all()
    }

    fn read_owned_directories(&self) -> Result<Vec<ProcessOwnedDirectory>, ProcessError> {
        let count = std::fs::read_to_string(self.ownership_journal().join("count"))
            .map_err(ProcessError::Spawn)?
            .parse::<usize>()
            .map_err(|_| ProcessError::Spawn(std::io::Error::other("invalid record count")))?;
        (0..count)
            .map(|index| {
                std::fs::read(self.ownership_journal().join(format!("record-{index}")))
                    .map_err(ProcessError::Spawn)
                    .and_then(|encoded| ProcessOwnedDirectory::decode(&encoded))
            })
            .collect()
    }

    fn persistent_target(
        &self,
        image: &[u8],
    ) -> Result<(ProductionProcessLaunchAuthority, PathBuf), Box<dyn std::error::Error>> {
        assert!(Path::new(BROKER_BINARY).is_file());
        self.make_root_private()?;
        let runtime = self.path.join("external-runtime");
        std::fs::create_dir(&runtime)?;
        let target = runtime.join("helper");
        std::fs::write(&target, image)?;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        let canonical_target = std::fs::canonicalize(&target)?;
        let length = u64::try_from(image.len())?;
        let digest: [u8; 32] = Sha256::digest(image).into();
        let authority = ProductionProcessLaunchAuthority::new(
            File::open(self.root())?,
            File::open(&canonical_target)?,
            canonical_target.clone(),
            File::open(self.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            ExecutableFileAttestation::new(length, digest),
            |records| self.persist_owned_directories(records),
        )?;
        Ok((authority, canonical_target))
    }

    fn external_source(&self, image: &[u8]) -> std::io::Result<(File, ExecutableFileAttestation)> {
        let path = self.path.join("external-executable");
        std::fs::write(&path, image)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let length = u64::try_from(image.len())
            .map_err(|_| std::io::Error::other("external test image is too large"))?;
        let digest: [u8; 32] = Sha256::digest(image).into();
        Ok((
            File::open(path)?,
            ExecutableFileAttestation::new(length, digest),
        ))
    }

    fn install_authority(
        &self,
    ) -> Result<ProductionProcessLaunchAuthority, Box<dyn std::error::Error>> {
        self.install_authority_with_image(b"#!/bin/sh\nexec /bin/sh \"$@\"\n")
    }

    fn install_authority_with_image(
        &self,
        image: &[u8],
    ) -> Result<ProductionProcessLaunchAuthority, Box<dyn std::error::Error>> {
        assert!(Path::new(BROKER_BINARY).is_file());
        let executable = self.root().join("bin/helper");
        std::fs::write(&executable, image)?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        Ok(ProductionProcessLaunchAuthority::new_disposable_canary(
            File::open(self.root())?,
            File::open(executable)?,
            PathBuf::from("bin/helper"),
            File::open(self.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            image,
        )?)
    }

    fn install_attested_authority_with_image(
        &self,
        image: &[u8],
    ) -> Result<ProductionProcessLaunchAuthority, Box<dyn std::error::Error>> {
        assert!(Path::new(BROKER_BINARY).is_file());
        let executable = self.root().join("bin/helper");
        std::fs::write(&executable, image)?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let length = u64::try_from(image.len())?;
        let digest: [u8; 32] = Sha256::digest(image).into();
        Ok(
            ProductionProcessLaunchAuthority::new_disposable_canary_from_attested_file(
                File::open(self.root())?,
                File::open(executable)?,
                PathBuf::from("bin/helper"),
                File::open(self.admitted_cwd())?,
                PathBuf::from("workspaces/admitted"),
                ExecutableFileAttestation::new(length, digest),
            )?,
        )
    }

    fn install_external_attested_authority_with_image(
        &self,
        image: &[u8],
    ) -> Result<ProductionProcessLaunchAuthority, Box<dyn std::error::Error>> {
        assert!(Path::new(BROKER_BINARY).is_file());
        self.make_root_private()?;
        let (executable, attestation) = self.external_source(image)?;
        Ok(
            ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
                File::open(self.root())?,
                executable,
                File::open(self.admitted_cwd())?,
                PathBuf::from("workspaces/admitted"),
                attestation,
            )?,
        )
    }
}

fn zero_digest(length: u64) -> Result<[u8; 32], std::io::Error> {
    let mut hasher = Sha256::new();
    let buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let buffer_length = u64::try_from(buffer.len())
        .map_err(|_| std::io::Error::other("test buffer length overflow"))?;
    let mut remaining = length;
    while remaining != 0 {
        let chunk_length = usize::try_from(remaining.min(buffer_length))
            .map_err(|_| std::io::Error::other("test chunk length overflow"))?;
        hasher.update(&buffer[..chunk_length]);
        let chunk_length = u64::try_from(chunk_length)
            .map_err(|_| std::io::Error::other("test chunk length conversion overflow"))?;
        remaining = remaining.saturating_sub(chunk_length);
    }
    Ok(hasher.finalize().into())
}

fn root_has_entry_with_prefix(case: &Case, prefix: &str) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(case.root())? {
        if entry?.file_name().to_string_lossy().starts_with(prefix) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn create_mode_controlled_fifo(path: &Path) -> std::io::Result<()> {
    let status = std::process::Command::new("/usr/bin/mkfifo")
        .arg(path)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "mkfifo exited with {status}"
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_fifo() || metadata.permissions().mode() & 0o777 != 0o755 {
        return Err(std::io::Error::other(
            "replacement path is not a mode-0755 FIFO",
        ));
    }
    Ok(())
}

fn replace_path_with_fifo(path: &Path) -> std::io::Result<()> {
    std::fs::rename(path, path.with_extension("retained"))?;
    create_mode_controlled_fifo(path)
}

fn replace_parent_mapping_with_fifo(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("target has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("target has no file name"))?;
    std::fs::rename(parent, parent.with_extension("retained"))?;
    std::fs::create_dir(parent)?;
    create_mode_controlled_fifo(&parent.join(name))
}

#[test]
fn persistent_production_enrollment_records_owned_directories_before_return()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("production-enrollment-recorded")?;
    let image = b"#!/bin/sh\nexit 0\n";
    let (authority, _) = case.persistent_target(image)?;
    let records = case.read_owned_directories()?;

    assert_eq!(records.len(), 2);
    assert_eq!(authority.owned_directories().len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.kind() != ProcessOwnedDirectoryKind::SealedExecutable)
    );
    Ok(())
}

#[test]
fn persistent_original_target_preserves_runtime_assets_and_bounded_inputs()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("persistent-original-runtime")?;
    let image = b"#!/bin/sh\nprintf '%s|%s|%s|' \"$1\" \"$NANIKA_TEST\" \"$PWD\"\ncat\nprintf '|'\ncat \"$(dirname \"$0\")/runtime-asset\"\n";
    let (authority, target) = case.persistent_target(image)?;
    std::fs::write(
        target
            .parent()
            .ok_or_else(|| std::io::Error::other("target has no parent"))?
            .join("runtime-asset"),
        b"adjacent-runtime",
    )?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("not-an-ambient-command"),
            OsString::from("argument"),
        ],
        Duration::from_secs(5),
    )?
    .with_env("NANIKA_TEST", "explicit")
    .with_stdin(b"stdin-data".to_vec());
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert!(report.is_success(), "report: {report:?}");
    assert_eq!(
        report.stdout,
        format!(
            "argument|explicit|{}|stdin-data|adjacent-runtime",
            std::fs::canonicalize(case.admitted_cwd())?.display()
        )
        .as_bytes()
    );
    let identity = report
        .kernel_identity
        .as_ref()
        .ok_or_else(|| std::io::Error::other("released target has no kernel identity"))?;
    assert!(report.direct_child_reaped && report.group_absent && report.cleanup_complete);
    assert!(!supervisor.has_owned_processes());
    assert!(matches!(
        inspect_recorded_process_identity(
            identity.pid(),
            identity.process_group_id(),
            identity.process_start_identity(),
        )?,
        RecordedProcessIdentityStatus::ExactGroupAbsent(_)
    ));
    Ok(())
}

#[derive(Clone, Copy)]
enum PersistentTargetMutation {
    Bytes,
    Namespace,
    Permissions,
    Fifo,
}

fn assert_persistent_target_mutation_is_refused(
    mutation: PersistentTargetMutation,
) -> Result<(), Box<dyn std::error::Error>> {
    let label = match mutation {
        PersistentTargetMutation::Bytes => "persistent-changed-bytes",
        PersistentTargetMutation::Namespace => "persistent-changed-namespace",
        PersistentTargetMutation::Permissions => "persistent-changed-permissions",
        PersistentTargetMutation::Fifo => "persistent-fifo-namespace",
    };
    let case = Case::create(label)?;
    let image = b"#!/bin/sh\nprintf target-ran >> \"$1\"\n";
    let (authority, target) = case.persistent_target(image)?;
    let side_effect = case.path.join("target-side-effect");
    let actor_target = target.clone();
    let actor_image = image.to_vec();
    let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
    let actor = thread::spawn(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let (request, started_authority) = gate_authority.receive()?;
            if matches!(mutation, PersistentTargetMutation::Fifo) {
                let identity = request.identity();
                let encoded = format!(
                    "{}\n{}\n{}\n",
                    identity.pid(),
                    identity.process_group_id(),
                    identity.process_start_identity()
                );
                write_synced_test_file(
                    &actor_target
                        .parent()
                        .and_then(Path::parent)
                        .ok_or_else(|| std::io::Error::other("target has no case parent"))?
                        .join(PERSISTENT_FIFO_BROKER_IDENTITY),
                    encoded.as_bytes(),
                )?;
            }
            match mutation {
                PersistentTargetMutation::Bytes => {
                    let mut changed = actor_image.clone();
                    let last = changed
                        .last_mut()
                        .ok_or_else(|| std::io::Error::other("empty target image"))?;
                    *last ^= 1;
                    std::fs::write(&actor_target, changed)?;
                    std::fs::set_permissions(
                        &actor_target,
                        std::fs::Permissions::from_mode(0o755),
                    )?;
                }
                PersistentTargetMutation::Namespace => {
                    std::fs::rename(&actor_target, actor_target.with_extension("retained"))?;
                    std::fs::write(&actor_target, &actor_image)?;
                    std::fs::set_permissions(
                        &actor_target,
                        std::fs::Permissions::from_mode(0o755),
                    )?;
                }
                PersistentTargetMutation::Permissions => {
                    std::fs::set_permissions(
                        &actor_target,
                        std::fs::Permissions::from_mode(0o775),
                    )?;
                }
                PersistentTargetMutation::Fifo => replace_path_with_fifo(&actor_target)?,
            }
            request.release_authorized()?;
            let started = started_authority.receive_started()?;
            started.persisted()?;
            Ok(())
        },
    );
    let spec = ProcessSpec::new(
        vec![
            OsString::from("not-an-ambient-command"),
            side_effect.clone().into_os_string(),
        ],
        Duration::from_secs(if matches!(mutation, PersistentTargetMutation::Fifo) {
            30
        } else {
            5
        }),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let outcome = supervisor.run_authorized(&spec, &authority, &mut gate)?;
    drop(gate);
    actor
        .join()
        .map_err(|_| std::io::Error::other("mutation gate actor panicked"))?
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    assert!(
        !matches!(outcome, AuthorizedProcessOutcome::Started(ref report) if report.is_success())
    );
    assert!(!side_effect.exists());
    assert!(!supervisor.has_owned_processes());
    if matches!(mutation, PersistentTargetMutation::Fifo) {
        let staging = authority
            .owned_directories()
            .into_iter()
            .find(|record| record.kind() == ProcessOwnedDirectoryKind::BrokerStaging)
            .map(|record| case.root().join(record.name()))
            .ok_or_else(|| std::io::Error::other("missing broker staging record"))?;
        assert!(std::fs::read_dir(staging)?.next().is_none());
    }
    Ok(())
}

#[test]
fn persistent_target_changed_bytes_before_release_are_refused()
-> Result<(), Box<dyn std::error::Error>> {
    assert_persistent_target_mutation_is_refused(PersistentTargetMutation::Bytes)
}

#[test]
fn persistent_target_changed_namespace_before_release_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    assert_persistent_target_mutation_is_refused(PersistentTargetMutation::Namespace)
}

#[test]
fn persistent_target_group_writable_mode_before_release_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    assert_persistent_target_mutation_is_refused(PersistentTargetMutation::Permissions)
}

const PERSISTENT_FIFO_CASE_ENV: &str = "NANIKA_PERSISTENT_FIFO_CASE";
const PERSISTENT_FIFO_MODE_ENV: &str = "NANIKA_PERSISTENT_FIFO_MODE";
const PERSISTENT_FIFO_BROKER_IDENTITY: &str = "broker-identity";

fn persistent_constructor_fifo_case(case_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let serialization = CASE_SERIALIZATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let case = Case {
        path: case_path,
        _serialization: serialization,
    };
    case.make_root_private()?;
    let image = b"#!/bin/sh\nprintf target-ran > \"$1\"\n";
    let runtime = case.path.join("external-runtime");
    std::fs::create_dir(&runtime)?;
    let target = runtime.join("helper");
    std::fs::write(&target, image)?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    let canonical_target = std::fs::canonicalize(&target)?;
    let executable = File::open(&canonical_target)?;
    let attestation =
        ExecutableFileAttestation::new(u64::try_from(image.len())?, Sha256::digest(image).into());
    let callback_target = canonical_target.clone();

    let result = ProductionProcessLaunchAuthority::new(
        File::open(case.root())?,
        executable,
        canonical_target,
        File::open(case.admitted_cwd())?,
        PathBuf::from("workspaces/admitted"),
        attestation,
        |records| {
            case.persist_owned_directories(records)?;
            replace_parent_mapping_with_fifo(&callback_target)?;
            Ok(())
        },
    );

    let Err(ProcessError::Spawn(error)) = result else {
        return Err(std::io::Error::other("FIFO replacement was admitted").into());
    };
    assert_eq!(
        error.to_string(),
        "process launch name no longer maps to its admitted identity"
    );
    assert_eq!(case.read_owned_directories()?.len(), 2);
    assert!(!case.path.join("target-side-effect").exists());
    for prefix in [".orchestrator-broker-", ".orchestrator-staging-"] {
        assert!(!root_has_entry_with_prefix(&case, prefix)?, "{prefix}");
    }
    Ok(())
}

#[test]
fn persistent_fifo_subprocess_helper() -> Result<(), Box<dyn std::error::Error>> {
    let Some(mode) = std::env::var_os(PERSISTENT_FIFO_MODE_ENV) else {
        return Ok(());
    };
    match mode.to_str() {
        Some("constructor") => {
            let case_path = std::env::var_os(PERSISTENT_FIFO_CASE_ENV)
                .ok_or_else(|| std::io::Error::other("missing persistent FIFO case path"))?;
            persistent_constructor_fifo_case(PathBuf::from(case_path))
        }
        Some("broker") => {
            assert_persistent_target_mutation_is_refused(PersistentTargetMutation::Fifo)
        }
        _ => Err(std::io::Error::other("unknown persistent FIFO subprocess mode").into()),
    }
}

fn run_bounded_persistent_fifo_subprocess(
    label: &str,
    mode: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create(label)?;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "persistent_fifo_subprocess_helper",
            "--nocapture",
        ])
        .env(PERSISTENT_FIFO_CASE_ENV, &case.path)
        .env(PERSISTENT_FIFO_MODE_ENV, mode)
        .spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "persistent FIFO subprocess exited with {status}"
                ))
                .into())
            };
        }
        if std::time::Instant::now() >= deadline {
            if mode == "constructor" {
                let _ = child.kill();
                let _ = child.wait();
            } else {
                let cleanup_deadline = std::time::Instant::now() + Duration::from_secs(25);
                while std::time::Instant::now() < cleanup_deadline {
                    if child.try_wait()?.is_some() {
                        return Err(std::io::Error::other(format!(
                            "persistent {mode} FIFO revalidation blocked"
                        ))
                        .into());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                let _ = child.kill();
                let _ = child.wait();
                clean_exact_fifo_broker_after_stuck_owner(&case)?;
            }
            return Err(std::io::Error::other(format!(
                "persistent {mode} FIFO revalidation blocked"
            ))
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn clean_exact_fifo_broker_after_stuck_owner(
    case: &Case,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = case.path.join(PERSISTENT_FIFO_BROKER_IDENTITY);
    let encoded = match std::fs::read_to_string(path) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut fields = encoded.lines();
    let pid = fields
        .next()
        .ok_or_else(|| std::io::Error::other("missing broker pid"))?
        .parse::<u32>()?;
    let pgid = fields
        .next()
        .ok_or_else(|| std::io::Error::other("missing broker process group"))?
        .parse::<u32>()?;
    let start = fields
        .next()
        .ok_or_else(|| std::io::Error::other("missing broker start identity"))?;
    if fields.next().is_some() {
        return Err(std::io::Error::other("unexpected broker identity fields").into());
    }
    match inspect_recorded_process_identity(pid, pgid, start)? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(_) => return Ok(()),
        RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {
            return Err(std::io::Error::other(
                "stuck FIFO broker identity no longer matches its surviving group",
            )
            .into());
        }
        RecordedProcessIdentityStatus::ExactLive => {}
    }
    if !matches!(
        inspect_recorded_process_identity(pid, pgid, start)?,
        RecordedProcessIdentityStatus::ExactLive
    ) {
        return Err(std::io::Error::other("stuck FIFO broker identity changed").into());
    }
    let raw_group = i32::try_from(pgid)?;
    let group = Pid::from_raw(raw_group)
        .ok_or_else(|| std::io::Error::other("invalid broker process group"))?;
    match kill_process_group(group, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match inspect_recorded_process_identity(pid, pgid, start)? {
            RecordedProcessIdentityStatus::ExactGroupAbsent(_) => return Ok(()),
            RecordedProcessIdentityStatus::ExactLive
            | RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::other("stuck FIFO broker cleanup timed out").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn persistent_constructor_fifo_revalidation_is_bounded_and_cleans_owned_state()
-> Result<(), Box<dyn std::error::Error>> {
    run_bounded_persistent_fifo_subprocess("persistent-constructor-fifo", "constructor")
}

#[test]
fn persistent_broker_fifo_refusal_and_cleanup_are_bounded() -> Result<(), Box<dyn std::error::Error>>
{
    run_bounded_persistent_fifo_subprocess("persistent-broker-fifo", "broker")
}

#[test]
fn persistent_enrollment_failure_cannot_produce_runnable_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("persistent-record-failure")?;
    case.make_root_private()?;
    let image = b"#!/bin/sh\nprintf target-ran > \"$1\"\n";
    let (executable, attestation) = case.external_source(image)?;
    let target = std::fs::canonicalize(case.path.join("external-executable"))?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    let callback_ran = std::cell::Cell::new(false);

    let result = ProductionProcessLaunchAuthority::new(
        File::open(case.root())?,
        executable,
        target,
        File::open(case.admitted_cwd())?,
        PathBuf::from("workspaces/admitted"),
        attestation,
        |records| {
            callback_ran.set(true);
            if records.len() != 2 {
                return Err(std::io::Error::other("unexpected ownership record count"));
            }
            Err(std::io::Error::other("injected durable journal failure"))
        },
    );

    assert!(matches!(result, Err(ProcessError::Spawn(_))));
    assert!(callback_ran.get());
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-broker-")?);
    assert!(!root_has_entry_with_prefix(
        &case,
        ".orchestrator-staging-"
    )?);
    Ok(())
}

#[test]
fn persistent_owner_close_leaves_reopen_recovery_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("persistent-clean-close")?;
    let (authority, target) = case.persistent_target(b"#!/bin/sh\nexit 0\n")?;
    let records = case.read_owned_directories()?;

    drop(authority);
    let recovered = recover_orphaned_process_directories(&File::open(case.root())?, &records)?;

    assert_eq!(recovered, 0);
    assert!(case.ownership_journal().is_dir());
    assert!(target.is_file());
    Ok(())
}

const PERSISTENT_CRASH_CASE_ENV: &str = "NANIKA_PERSISTENT_LAUNCH_CRASH_CASE";

fn write_synced_test_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    File::open(path)?.sync_all()
}

#[test]
fn persistent_owner_crash_subprocess_helper() -> Result<(), Box<dyn std::error::Error>> {
    let Some(case_path) = std::env::var_os(PERSISTENT_CRASH_CASE_ENV) else {
        return Ok(());
    };
    let serialization = CASE_SERIALIZATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let case = Case {
        path: PathBuf::from(case_path),
        _serialization: serialization,
    };
    let image = b"#!/bin/sh\nprintf target-ran > \"$1\"\n";
    let (authority, _) = case.persistent_target(image)?;
    let actor_path = case.path.clone();
    let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
    let _actor = thread::spawn(move || {
        let Ok((request, _started_authority)) = gate_authority.receive() else {
            return;
        };
        let identity = request.identity();
        let encoded = format!(
            "{}\n{}\n{}\n",
            identity.pid(),
            identity.process_group_id(),
            identity.process_start_identity()
        );
        if write_synced_test_file(&actor_path.join("broker-identity"), encoded.as_bytes()).is_err()
            || write_synced_test_file(&actor_path.join("owner-ready"), b"ready").is_err()
        {
            return;
        }
        loop {
            thread::park();
        }
    });
    let spec = ProcessSpec::new(
        vec![
            OsString::from("not-an-ambient-command"),
            case.path.join("target-side-effect").into_os_string(),
        ],
        Duration::from_secs(30),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;
    let _outcome = supervisor.run_authorized(&spec, &authority, &mut gate)?;
    Err(std::io::Error::other("crash helper returned before owner death").into())
}

#[test]
fn persistent_outer_and_gate_namespaces_recover_after_owner_crash()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("persistent-owner-crash")?;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "persistent_owner_crash_subprocess_helper",
            "--nocapture",
        ])
        .env(PERSISTENT_CRASH_CASE_ENV, &case.path)
        .spawn()?;
    let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !case.path.join("owner-ready").is_file() {
        if std::time::Instant::now() >= ready_deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("crash helper did not reach the start gate").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    let records = case.read_owned_directories()?;
    let staging = records
        .iter()
        .find(|record| record.kind() == ProcessOwnedDirectoryKind::BrokerStaging)
        .map(|record| case.root().join(record.name()))
        .ok_or_else(|| std::io::Error::other("missing persisted staging record"))?;
    let request = std::fs::read_dir(&staging)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".orchestrator-request-"))
        })
        .ok_or_else(|| std::io::Error::other("missing persisted broker request"))?;
    assert!(request.join("gate-root").is_file());
    let identity = std::fs::read_to_string(case.path.join("broker-identity"))?;
    let mut fields = identity.lines();
    let pid = fields
        .next()
        .ok_or_else(|| std::io::Error::other("missing broker pid"))?
        .parse::<u32>()?;
    let pgid = fields
        .next()
        .ok_or_else(|| std::io::Error::other("missing broker pgid"))?
        .parse::<u32>()?;
    let start_identity = fields
        .next()
        .ok_or_else(|| std::io::Error::other("missing broker start identity"))?
        .to_owned();

    child.kill()?;
    let owner_status = child.wait()?;
    assert!(!owner_status.success());
    let absence_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match inspect_recorded_process_identity(pid, pgid, &start_identity)? {
            RecordedProcessIdentityStatus::ExactGroupAbsent(_) => break,
            RecordedProcessIdentityStatus::ExactLive
            | RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {
                if std::time::Instant::now() >= absence_deadline {
                    return Err(std::io::Error::other(
                        "broker process group remained after owner crash",
                    )
                    .into());
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    let first = recover_orphaned_process_directories(&File::open(case.root())?, &records)?;
    let second = recover_orphaned_process_directories(&File::open(case.root())?, &records)?;

    assert_eq!((first, second), (2, 0));
    assert!(!case.path.join("target-side-effect").exists());
    assert!(case.ownership_journal().is_dir());
    Ok(())
}

#[test]
fn attested_canary_streams_an_executable_larger_than_the_legacy_memory_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("large-attested-executable")?;
    let length = (64_u64 * 1024 * 1024) + 1;
    let executable = case.root().join("bin/helper");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&executable)?;
    file.set_len(length)?;
    file.sync_all()?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let attestation = ExecutableFileAttestation::new(length, zero_digest(length)?);

    let authority = ProductionProcessLaunchAuthority::new_disposable_canary_from_attested_file(
        File::open(case.root())?,
        File::open(executable)?,
        PathBuf::from("bin/helper"),
        File::open(case.admitted_cwd())?,
        PathBuf::from("workspaces/admitted"),
        attestation,
    )?;

    assert_eq!(attestation.length(), length);
    assert!(
        authority
            .owned_directories()
            .iter()
            .any(|record| { record.kind() == ProcessOwnedDirectoryKind::SealedExecutable })
    );
    Ok(())
}

#[test]
fn attested_canary_rejects_out_of_range_lengths_before_creating_process_owned_state()
-> Result<(), Box<dyn std::error::Error>> {
    for (label, length) in [
        ("zero-attested-length", 0),
        (
            "over-max-attested-length",
            MAX_ATTESTED_EXECUTABLE_BYTES + 1,
        ),
    ] {
        let case = Case::create(label)?;
        let executable = case.root().join("bin/helper");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable)?;
        file.set_len(length)?;
        file.sync_all()?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;

        let result = ProductionProcessLaunchAuthority::new_disposable_canary_from_attested_file(
            File::open(case.root())?,
            File::open(executable)?,
            PathBuf::from("bin/helper"),
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            ExecutableFileAttestation::new(length, [0_u8; 32]),
        );

        let Err(orchestrator_process::ProcessError::Spawn(error)) = result else {
            return Err(std::io::Error::other(format!(
                "out-of-range attestation length {length} was accepted"
            ))
            .into());
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        for prefix in [
            ".orchestrator-launch-",
            ".orchestrator-broker-",
            ".orchestrator-staging-",
        ] {
            assert!(!root_has_entry_with_prefix(&case, prefix)?, "{prefix}");
        }
    }
    Ok(())
}

#[test]
fn attested_canary_rejects_wrong_length_without_creating_a_sealed_clone()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("wrong-attested-length")?;
    let image = b"#!/bin/sh\nexit 0\n";
    let executable = case.root().join("bin/helper");
    std::fs::write(&executable, image)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let digest: [u8; 32] = Sha256::digest(image).into();

    let result = ProductionProcessLaunchAuthority::new_disposable_canary_from_attested_file(
        File::open(case.root())?,
        File::open(executable)?,
        PathBuf::from("bin/helper"),
        File::open(case.admitted_cwd())?,
        PathBuf::from("workspaces/admitted"),
        ExecutableFileAttestation::new(u64::try_from(image.len())? + 1, digest),
    );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn attested_canary_rejects_wrong_digest_and_removes_the_partial_clone()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("wrong-attested-digest")?;
    let image = b"#!/bin/sh\nexit 0\n";
    let executable = case.root().join("bin/helper");
    std::fs::write(&executable, image)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let mut digest: [u8; 32] = Sha256::digest(image).into();
    digest[0] ^= 1;

    let result = ProductionProcessLaunchAuthority::new_disposable_canary_from_attested_file(
        File::open(case.root())?,
        File::open(executable)?,
        PathBuf::from("bin/helper"),
        File::open(case.admitted_cwd())?,
        PathBuf::from("workspaces/admitted"),
        ExecutableFileAttestation::new(u64::try_from(image.len())?, digest),
    );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn attested_canary_launch_ignores_ambient_path_and_uses_the_sealed_target()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("attested-normal-gate-launch")?;
    let authority =
        case.install_attested_authority_with_image(b"#!/bin/sh\nexec /bin/sh \"$@\"\n")?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("definitely-not-an-ambient-path-command"),
            OsString::from("-c"),
            OsString::from("printf attested-canary"),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert!(report.is_success(), "report: {report:?}");
    assert_eq!(report.stdout, b"attested-canary");
    Ok(())
}

#[test]
fn external_attested_canary_uses_the_open_file_after_its_source_path_is_replaced()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-source-path-replaced")?;
    case.make_root_private()?;
    let admitted = b"#!/bin/sh\nexec /bin/sh \"$@\"\n";
    let source_path = case.path.join("external-executable");
    let (source, attestation) = case.external_source(admitted)?;
    std::fs::rename(&source_path, case.path.join("retained-executable"))?;
    std::fs::write(&source_path, b"#!/bin/sh\nexit 91\n")?;
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o700))?;

    let authority =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            attestation,
        )?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("external-retained-command"),
            OsString::from("-c"),
            OsString::from("printf retained-source"),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert!(report.is_success(), "report: {report:?}");
    assert_eq!(report.stdout, b"retained-source");
    assert!(std::fs::read_dir(case.root().join("bin"))?.next().is_none());
    Ok(())
}

#[test]
fn external_attested_canary_streams_a_large_file_without_source_staging()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-large-no-source-staging")?;
    case.make_root_private()?;
    let length = (64_u64 * 1024 * 1024) + 1;
    let source_path = case.path.join("external-executable");
    let source = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&source_path)?;
    source.set_len(length)?;
    source.sync_all()?;
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o700))?;

    let authority =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            File::open(&source_path)?,
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            ExecutableFileAttestation::new(length, zero_digest(length)?),
        )?;

    assert!(std::fs::read_dir(case.root().join("bin"))?.next().is_none());
    assert!(
        authority
            .owned_directories()
            .iter()
            .any(|record| record.kind() == ProcessOwnedDirectoryKind::SealedExecutable)
    );
    Ok(())
}

#[test]
fn external_attested_canary_rejects_zero_and_over_max_lengths_without_owned_residue()
-> Result<(), Box<dyn std::error::Error>> {
    for (label, length) in [
        ("external-zero-length", 0),
        (
            "external-over-max-length",
            MAX_ATTESTED_EXECUTABLE_BYTES + 1,
        ),
    ] {
        let case = Case::create(label)?;
        case.make_root_private()?;
        let (source, _) = case.external_source(b"#!/bin/sh\nexit 0\n")?;

        let result =
            ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
                File::open(case.root())?,
                source,
                File::open(case.admitted_cwd())?,
                PathBuf::from("workspaces/admitted"),
                ExecutableFileAttestation::new(length, [0_u8; 32]),
            );

        let Err(orchestrator_process::ProcessError::Spawn(error)) = result else {
            return Err(std::io::Error::other(format!(
                "out-of-range external attestation length {length} was accepted"
            ))
            .into());
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        for prefix in [
            ".orchestrator-launch-",
            ".orchestrator-broker-",
            ".orchestrator-staging-",
        ] {
            assert!(!root_has_entry_with_prefix(&case, prefix)?, "{prefix}");
        }
    }
    Ok(())
}

#[test]
fn external_attested_canary_rejects_a_non_executable_source_without_owned_residue()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-source-mode")?;
    case.make_root_private()?;
    let image = b"#!/bin/sh\nexit 0\n";
    let (source, attestation) = case.external_source(image)?;
    std::fs::set_permissions(
        case.path.join("external-executable"),
        std::fs::Permissions::from_mode(0o600),
    )?;

    let result =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            attestation,
        );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn external_attested_canary_rejects_a_wrong_digest_without_owned_residue()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-source-digest")?;
    case.make_root_private()?;
    let image = b"#!/bin/sh\nexit 0\n";
    let (source, attestation) = case.external_source(image)?;
    let mut wrong_digest = attestation.sha256();
    wrong_digest[0] ^= 1;

    let result =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            ExecutableFileAttestation::new(attestation.length(), wrong_digest),
        );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn external_attested_canary_rejects_a_wrong_length_without_owned_residue()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-source-length")?;
    case.make_root_private()?;
    let image = b"#!/bin/sh\nexit 0\n";
    let (source, attestation) = case.external_source(image)?;

    let result =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            ExecutableFileAttestation::new(attestation.length() + 1, attestation.sha256()),
        );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn external_attested_canary_rejects_a_cwd_symlink_escape_before_creating_owned_state()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-cwd-symlink")?;
    case.make_root_private()?;
    let (source, attestation) = case.external_source(b"#!/bin/sh\nexit 0\n")?;
    std::os::unix::fs::symlink(
        case.path.join("outside"),
        case.root().join("workspaces/alias"),
    )?;

    let result =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.path.join("outside"))?,
            PathBuf::from("workspaces/alias"),
            attestation,
        );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn external_attested_canary_rejects_a_cwd_identity_alias_before_creating_owned_state()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-cwd-identity")?;
    case.make_root_private()?;
    let (source, attestation) = case.external_source(b"#!/bin/sh\nexit 0\n")?;

    let result =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.path.join("outside"))?,
            PathBuf::from("workspaces/admitted"),
            attestation,
        );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn external_attested_canary_rejects_a_non_private_root_before_creating_owned_state()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-root-mode")?;
    std::fs::set_permissions(case.root(), std::fs::Permissions::from_mode(0o755))?;
    let (source, attestation) = case.external_source(b"#!/bin/sh\nexit 0\n")?;

    let result =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            attestation,
        );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(!root_has_entry_with_prefix(&case, ".orchestrator-launch-")?);
    Ok(())
}

#[test]
fn external_attested_canary_rejects_stale_owned_state_without_deleting_it()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-stale-root")?;
    case.make_root_private()?;
    let stale = case.root().join(".orchestrator-launch-999-999");
    std::fs::create_dir(&stale)?;
    let (source, attestation) = case.external_source(b"#!/bin/sh\nexit 0\n")?;

    let result =
        ProductionProcessLaunchAuthority::new_disposable_canary_from_external_attested_file(
            File::open(case.root())?,
            source,
            File::open(case.admitted_cwd())?,
            PathBuf::from("workspaces/admitted"),
            attestation,
        );

    assert!(matches!(
        result,
        Err(orchestrator_process::ProcessError::Spawn(_))
    ));
    assert!(stale.is_dir());
    assert_eq!(
        process_owned_names(&case.root(), ".orchestrator-launch-")?,
        vec![OsString::from(".orchestrator-launch-999-999")]
    );
    Ok(())
}

#[test]
fn external_attested_canary_rechecks_private_root_mode_before_launch()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-root-mode-tamper")?;
    let authority =
        case.install_external_attested_authority_with_image(b"#!/bin/sh\nexec /bin/sh \"$@\"\n")?;
    std::fs::set_permissions(case.root(), std::fs::Permissions::from_mode(0o755))?;
    let marker = case.path.join("root-mode-target-ran");
    let spec = ProcessSpec::new(
        vec![
            OsString::from("external-retained-command"),
            OsString::from("-c"),
            OsString::from(format!("touch '{}'", marker.display())),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let result = run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    );

    let Err(error) = result else {
        return Err(std::io::Error::other("tampered root was not rejected before spawn").into());
    };
    let error = error
        .downcast_ref::<AuthorizedProcessError>()
        .ok_or("pre-spawn failure lost its public production classification")?;
    assert_eq!(
        error.classification(),
        AuthorizedProcessErrorClassification::ProvenNotStarted
    );
    assert!(std::error::Error::source(error).is_none());
    assert_eq!(
        error.to_string(),
        "authorized process did not produce a process outcome"
    );
    assert!(!format!("{error:?}").contains(&case.root().to_string_lossy().into_owned()));
    assert!(!marker.exists());
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

#[test]
fn external_attested_canary_drop_removes_every_owned_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("external-owned-cleanup")?;
    let authority = case.install_external_attested_authority_with_image(b"#!/bin/sh\nexit 0\n")?;
    let owned_names = authority
        .owned_directories()
        .iter()
        .map(|record| record.name().to_os_string())
        .collect::<Vec<_>>();
    assert_eq!(owned_names.len(), 3);

    drop(authority);

    assert!(
        owned_names
            .iter()
            .all(|name| !case.root().join(name).exists())
    );
    assert!(case.path.join("external-executable").is_file());
    Ok(())
}

#[test]
fn production_broker_preserves_genuine_target_exit_126() -> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("target-exit-126")?;
    let authority = case.install_authority()?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from("exit 126"),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert_eq!(report.termination, ProcessTermination::Exited(126));
    assert!(report.cleanup_complete, "report: {report:?}");
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

#[test]
fn production_target_cannot_run_before_durable_identity_release()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("identity-release-order")?;
    let authority = case.install_authority()?;
    let marker = case.path.join("target-ran");
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from(format!("touch '{}'; printf '%s' $$", marker.display())),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;
    let binding_bytes = [0xa5; 32];
    let (mut gate, gate_authority) =
        ProcessStartGate::channel(ProcessRequestBinding::from_bytes(binding_bytes));
    let runner = thread::spawn(move || supervisor.run_authorized(&spec, &authority, &mut gate));

    let (request, started_authority) = gate_authority.receive()?;
    assert!(request.request_binding().matches_bytes(&binding_bytes));
    let pid = request.identity().pid();
    let process_group_id = request.identity().process_group_id();
    let start_identity = request.identity().process_start_identity().to_owned();
    assert_eq!(pid, process_group_id);
    assert!(start_identity.starts_with("darwin:"));
    assert!(
        !marker.exists(),
        "target executed before durable identity callback authorized release"
    );
    request.release_authorized()?;
    let started_receipt = started_authority.receive_started()?;
    assert!(started_receipt.receipt().matches_request(&binding_bytes));
    assert!(
        !started_receipt.receipt().matches_request(&[0x5a; 32]),
        "started receipt accepted a substituted request fingerprint"
    );
    let post_grant_identity = started_receipt.receipt().identity();
    assert_eq!(post_grant_identity.pid(), pid);
    assert_eq!(post_grant_identity.process_group_id(), process_group_id);
    assert_eq!(post_grant_identity.process_start_identity(), start_identity);
    assert!(
        !marker.exists(),
        "target executed before the started receipt committed"
    );
    started_receipt.persisted()?;
    let report = started(
        runner
            .join()
            .map_err(|_| std::io::Error::other("process runner panicked"))??,
    )?;

    assert!(report.is_success(), "report: {report:?}");
    assert_eq!(std::str::from_utf8(&report.stdout)?.parse::<u32>()?, pid);
    assert!(marker.is_file());
    Ok(())
}

#[test]
fn rejected_durable_identity_proves_target_not_started_and_closes_ownership()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("identity-rejected")?;
    let authority = case.install_authority()?;
    let marker = case.path.join("rejected-target-ran");
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from(format!("touch '{}'", marker.display())),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let outcome =
        run_with_gate_decision(&supervisor, &spec, &authority, TestGateDecision::Rejected)?;

    let AuthorizedProcessOutcome::NotStarted(receipt) = outcome else {
        return Err(
            std::io::Error::other("rejected gate was not classified as not-started").into(),
        );
    };
    assert_eq!(receipt.reason, ProcessNotStartedReason::GateRejected);
    assert!(receipt.launcher_spawned);
    let identity = receipt
        .launcher_identity
        .as_ref()
        .ok_or("rejected gate lost its observed launcher identity")?;
    let start_identity = identity.process_start_identity();
    let debug = format!("{receipt:?}");
    assert!(debug.contains("launcher_identity: Some(\"REDACTED\")"));
    assert!(!debug.contains(start_identity));
    assert!(!marker.exists());
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

#[test]
fn indeterminate_durable_identity_proves_target_not_started_and_closes_ownership()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("identity-indeterminate")?;
    let authority = case.install_authority()?;
    let marker = case.path.join("indeterminate-target-ran");
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from(format!("touch '{}'", marker.display())),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let outcome = run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::Indeterminate,
    )?;

    let AuthorizedProcessOutcome::NotStarted(receipt) = outcome else {
        return Err(
            std::io::Error::other("indeterminate gate was not classified as not-started").into(),
        );
    };
    assert_eq!(receipt.reason, ProcessNotStartedReason::GateIndeterminate);
    assert!(receipt.launcher_spawned);
    assert!(receipt.launcher_identity.is_some());
    assert!(!marker.exists());
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

#[test]
fn cancellation_before_late_actor_reply_never_starts_target()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("cancel-before-grant")?;
    let authority = case.install_authority()?;
    let marker = case.path.join("cancelled-target-ran");
    let cancellation = CancellationToken::new();
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from(format!("touch '{}'", marker.display())),
        ],
        Duration::from_secs(5),
    )?
    .with_cancellation(cancellation.clone());
    let supervisor = ProcessSupervisor::new(1)?;
    let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
    let runner = thread::spawn(move || {
        let outcome = supervisor.run_authorized(&spec, &authority, &mut gate);
        (outcome, supervisor)
    });

    let (request, _started_authority) = gate_authority.receive()?;
    assert!(cancellation.cancel());
    assert!(request.release_authorized().is_err());
    let (outcome, supervisor) = runner
        .join()
        .map_err(|_| std::io::Error::other("process runner panicked"))?;
    let AuthorizedProcessOutcome::NotStarted(receipt) = outcome? else {
        return Err(std::io::Error::other("pre-grant cancellation was not not-started").into());
    };
    assert_eq!(receipt.reason, ProcessNotStartedReason::Cancelled);
    assert!(receipt.cancellation_observed);
    assert!(receipt.launcher_identity.is_some());
    assert!(!marker.exists());
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

#[test]
fn deadline_interrupts_an_unanswered_durable_actor_request()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("deadline-before-actor-reply")?;
    let authority = case.install_authority()?;
    let marker = case.path.join("deadline-target-ran");
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from(format!("touch '{}'", marker.display())),
        ],
        Duration::from_secs(5),
    )?
    .with_term_grace(Duration::from_millis(25));
    let supervisor = ProcessSupervisor::new(1)?;
    let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
    let runner = thread::spawn(move || {
        let outcome = supervisor.run_authorized(&spec, &authority, &mut gate);
        (outcome, supervisor)
    });

    let (request, _started_authority) = gate_authority.receive()?;
    let (outcome, supervisor) = runner
        .join()
        .map_err(|_| std::io::Error::other("process runner panicked"))?;
    assert!(request.release_authorized().is_err());
    let AuthorizedProcessOutcome::NotStarted(receipt) = outcome? else {
        return Err(std::io::Error::other("deadline did not prove target not-started").into());
    };
    assert_eq!(receipt.reason, ProcessNotStartedReason::Deadline);
    assert!(receipt.deadline_observed);
    assert!(receipt.launcher_identity.is_some());
    assert!(!marker.exists());
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

#[test]
fn production_broker_preserves_other_target_exit_and_signal_statuses()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("target-statuses")?;
    let authority = case.install_authority()?;
    let supervisor = ProcessSupervisor::new(1)?;
    for (script, expected) in [
        ("exit 7", ProcessTermination::Exited(7)),
        ("kill -TERM $$", ProcessTermination::Signaled(15)),
    ] {
        let spec = ProcessSpec::new(
            vec![
                OsString::from("admitted-helper"),
                OsString::from("-c"),
                OsString::from(script),
            ],
            Duration::from_secs(5),
        )?;

        let report = started(run_with_gate_decision(
            &supervisor,
            &spec,
            &authority,
            TestGateDecision::ReleaseAuthorized,
        )?)?;

        assert_eq!(report.termination, expected, "report: {report:?}");
        assert!(report.cleanup_complete, "report: {report:?}");
        assert!(!supervisor.has_owned_processes());
    }
    Ok(())
}

#[test]
fn production_broker_classifies_exec_failure_as_infrastructure_error()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("exec-failure")?;
    let authority =
        case.install_authority_with_image(b"#!/definitely/missing/nanika-interpreter\n")?;
    let spec = ProcessSpec::new(
        vec![OsString::from("admitted-helper")],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert_eq!(
        report.termination,
        ProcessTermination::InfrastructureError,
        "report: {report:?}"
    );
    assert!(report.cleanup_complete, "report: {report:?}");
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

impl Drop for Case {
    fn drop(&mut self) {
        make_tree_removable(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn make_tree_removable(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(Result::ok) {
            make_tree_removable(&entry.path());
        }
    }
}

fn process_owned_names(root: &Path, prefix: &str) -> std::io::Result<Vec<OsString>> {
    Ok(std::fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with(prefix))
        .collect())
}

#[test]
fn production_broker_preserves_cwd_environment_stdin_and_pipes()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("roundtrip")?;
    let authority = case.install_authority()?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from("printf '%s|%s|' \"$NANIKA_TEST\" \"$PWD\"; cat; printf err >&2"),
        ],
        Duration::from_secs(5),
    )?
    .with_env("NANIKA_TEST", "allowed")
    .with_stdin(b"payload".to_vec());
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert!(report.is_success(), "report: {report:?}");
    assert_eq!(
        report.stdout,
        format!(
            "allowed|{}|payload",
            std::fs::canonicalize(case.admitted_cwd())?.display()
        )
        .as_bytes()
    );
    assert_eq!(report.stderr, b"err");
    let staging = process_owned_names(&case.root(), ".orchestrator-staging-")?;
    assert_eq!(staging.len(), 1);
    assert!(
        std::fs::read_dir(case.root().join(&staging[0]))?
            .next()
            .is_none()
    );
    assert_eq!(authority.owned_directories().len(), 3);
    Ok(())
}

#[test]
fn production_broker_does_not_leak_private_descriptors_to_the_target()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("descriptor-closure")?;
    let authority = case.install_authority()?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from(
                "for fd in 3 4 5 6 7 8 9 10 11 12 13 14 15 16; do \
                 if [ -e /dev/fd/$fd ]; then printf '%s\\n' \"$fd\"; fi; done",
            ),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert!(report.is_success(), "report: {report:?}");
    assert!(
        report.stdout.is_empty(),
        "target inherited private descriptors: {:?}",
        String::from_utf8_lossy(&report.stdout)
    );
    Ok(())
}

#[test]
fn production_broker_does_not_leak_status_environment_to_the_target()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("status-environment")?;
    let authority = case.install_authority()?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from(
                "if [ \"${NANIKA_BROKER_STATUS_PATH+x}\" = x ] || \
                 [ \"${NANIKA_BROKER_STATUS_DEVICE+x}\" = x ] || \
                 [ \"${NANIKA_BROKER_STATUS_INODE+x}\" = x ]; then \
                 printf leaked; fi",
            ),
        ],
        Duration::from_secs(5),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let report = started(run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?)?;

    assert!(report.is_success(), "report: {report:?}");
    assert!(report.stdout.is_empty(), "report: {report:?}");
    Ok(())
}

#[test]
fn recovery_fails_closed_on_an_unrecorded_valid_process_namespace()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("recovery")?;
    let authority = case.install_authority()?;
    let records = authority
        .owned_directories()
        .iter()
        .map(|record| ProcessOwnedDirectory::decode(&record.encode()))
        .collect::<Result<Vec<_>, _>>()?;
    let lookalike = case.root().join(".orchestrator-launch-999-999");
    std::fs::create_dir(&lookalike)?;
    std::fs::write(lookalike.join("executable"), b"not-owned")?;
    std::fs::set_permissions(
        lookalike.join("executable"),
        std::fs::Permissions::from_mode(0o500),
    )?;
    std::fs::set_permissions(&lookalike, std::fs::Permissions::from_mode(0o500))?;
    assert!(
        records
            .iter()
            .any(|record| { record.kind() == ProcessOwnedDirectoryKind::SealedExecutable })
    );
    let staging_name = records
        .iter()
        .find(|record| record.kind() == ProcessOwnedDirectoryKind::BrokerStaging)
        .map(|record| record.name().to_os_string())
        .ok_or_else(|| std::io::Error::other("missing broker staging ownership record"))?;
    let orphan = case
        .root()
        .join(staging_name)
        .join(".orchestrator-request-999-1");
    std::fs::create_dir(&orphan)?;
    std::fs::write(orphan.join("request"), b"bounded-request")?;
    std::fs::set_permissions(
        orphan.join("request"),
        std::fs::Permissions::from_mode(0o400),
    )?;
    std::fs::set_permissions(&orphan, std::fs::Permissions::from_mode(0o500))?;
    std::mem::forget(authority);

    let result = recover_orphaned_process_directories(&File::open(case.root())?, &records);

    assert!(result.is_err());
    assert!(lookalike.is_dir());
    assert!(orphan.is_dir());
    assert!(
        records
            .iter()
            .all(|record| case.root().join(record.name()).is_dir())
    );
    Ok(())
}

#[test]
fn recovery_fails_closed_on_every_malformed_process_namespace_prefix()
-> Result<(), Box<dyn std::error::Error>> {
    for (index, malformed) in [
        ".orchestrator-launch-invalid",
        ".orchestrator-broker-1",
        ".orchestrator-staging-1-2-extra",
        ".orchestrator-request--2",
    ]
    .into_iter()
    .enumerate()
    {
        let case = Case::create(&format!("malformed-recovery-{index}"))?;
        let authority = case.install_authority()?;
        let records = authority.owned_directories();
        let residue = case.root().join(malformed);
        std::fs::create_dir(&residue)?;
        std::mem::forget(authority);

        let result = recover_orphaned_process_directories(&File::open(case.root())?, &records);

        assert!(
            result.is_err(),
            "malformed namespace was accepted: {malformed}"
        );
        assert!(residue.is_dir());
        assert!(
            records
                .iter()
                .all(|record| case.root().join(record.name()).is_dir())
        );
    }
    Ok(())
}

#[test]
fn recovery_is_idempotent_after_an_exact_record_was_already_removed()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("idempotent-recovery")?;
    let authority = case.install_authority()?;
    let records = authority.owned_directories();
    let already_removed = records
        .iter()
        .find(|record| record.kind() == ProcessOwnedDirectoryKind::SealedExecutable)
        .map(|record| case.root().join(record.name()))
        .ok_or_else(|| std::io::Error::other("missing sealed executable record"))?;
    let unrelated = case.root().join("user-owned-data");
    std::fs::write(&unrelated, b"preserve")?;
    std::mem::forget(authority);
    std::fs::set_permissions(&already_removed, std::fs::Permissions::from_mode(0o700))?;
    std::fs::remove_file(already_removed.join("executable"))?;
    std::fs::remove_dir(&already_removed)?;

    let first = recover_orphaned_process_directories(&File::open(case.root())?, &records)?;
    let second = recover_orphaned_process_directories(&File::open(case.root())?, &records)?;

    assert_eq!(first, 2);
    assert_eq!(second, 0);
    assert_eq!(std::fs::read(unrelated)?, b"preserve");
    Ok(())
}

#[test]
fn recovery_accepts_a_nested_request_interrupted_during_creation()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("nested-request-creation")?;
    let authority = case.install_authority()?;
    let records = authority.owned_directories();
    let staging = records
        .iter()
        .find(|record| record.kind() == ProcessOwnedDirectoryKind::BrokerStaging)
        .map(|record| case.root().join(record.name()))
        .ok_or_else(|| std::io::Error::other("missing broker staging record"))?;
    let partial_request = staging.join(".orchestrator-request-999-1");
    std::fs::create_dir(&partial_request)?;
    std::fs::set_permissions(&partial_request, std::fs::Permissions::from_mode(0o700))?;
    std::mem::forget(authority);

    let recovered = recover_orphaned_process_directories(&File::open(case.root())?, &records)?;

    assert_eq!(recovered, 3);
    assert!(!partial_request.exists());
    Ok(())
}

#[test]
fn recovery_rejects_finalized_requests_without_a_valid_terminal_status()
-> Result<(), Box<dyn std::error::Error>> {
    for (label, status) in [
        ("missing", None),
        ("truncated", Some(b"NANBST01P".as_slice())),
        ("invalid", Some(b"INVALID!P\0\0\0\0".as_slice())),
    ] {
        let case = Case::create(&format!("invalid-final-status-{label}"))?;
        let authority = case.install_authority()?;
        let records = authority
            .owned_directories()
            .iter()
            .map(|record| ProcessOwnedDirectory::decode(&record.encode()))
            .collect::<Result<Vec<_>, _>>()?;
        let staging_name = records
            .iter()
            .find(|record| record.kind() == ProcessOwnedDirectoryKind::BrokerStaging)
            .map(|record| record.name().to_os_string())
            .ok_or_else(|| std::io::Error::other("missing broker staging record"))?;
        let request = case
            .root()
            .join(staging_name)
            .join(".orchestrator-request-999-1");
        std::fs::create_dir(&request)?;
        std::fs::write(request.join("request"), b"bounded-request")?;
        std::fs::set_permissions(
            request.join("request"),
            std::fs::Permissions::from_mode(0o400),
        )?;
        if let Some(status) = status {
            std::fs::write(request.join("status"), status)?;
            std::fs::set_permissions(
                request.join("status"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        std::fs::set_permissions(&request, std::fs::Permissions::from_mode(0o500))?;
        std::mem::forget(authority);

        let result = recover_orphaned_process_directories(&File::open(case.root())?, &records);

        assert!(
            result.is_err(),
            "invalid finalized status was accepted: {label}"
        );
        assert!(request.is_dir(), "failed recovery deleted residue: {label}");
    }
    Ok(())
}

#[test]
fn recovery_rejects_writable_top_level_sealed_directories() -> Result<(), Box<dyn std::error::Error>>
{
    for kind in [
        ProcessOwnedDirectoryKind::SealedExecutable,
        ProcessOwnedDirectoryKind::SealedBroker,
    ] {
        let case = Case::create(match kind {
            ProcessOwnedDirectoryKind::SealedExecutable => "writable-target-seal",
            ProcessOwnedDirectoryKind::SealedBroker => "writable-broker-seal",
            ProcessOwnedDirectoryKind::BrokerRequest | ProcessOwnedDirectoryKind::BrokerStaging => {
                unreachable!()
            }
        })?;
        let authority = case.install_authority()?;
        let records = authority.owned_directories();
        let writable = records
            .iter()
            .find(|record| record.kind() == kind)
            .map(|record| case.root().join(record.name()))
            .ok_or_else(|| std::io::Error::other("missing sealed directory record"))?;
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o700))?;
        std::mem::forget(authority);

        let result = recover_orphaned_process_directories(&File::open(case.root())?, &records);

        assert!(
            result.is_err(),
            "writable sealed directory was accepted: {kind:?}"
        );
        assert!(writable.is_dir());
        assert!(
            records
                .iter()
                .all(|record| case.root().join(record.name()).is_dir())
        );
    }
    Ok(())
}

#[test]
fn recovery_never_deletes_an_exact_name_with_the_wrong_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("recovery-identity-substitution")?;
    let authority = case.install_authority()?;
    let records = authority.owned_directories();
    let exact = records
        .iter()
        .find(|record| record.kind() == ProcessOwnedDirectoryKind::SealedExecutable)
        .map(|record| case.root().join(record.name()))
        .ok_or_else(|| std::io::Error::other("missing sealed executable record"))?;
    let original = case.root().join("held-original-seal");
    std::fs::set_permissions(&exact, std::fs::Permissions::from_mode(0o700))?;
    std::fs::rename(&exact, &original).map_err(|error| {
        std::io::Error::new(error.kind(), format!("rename original seal: {error}"))
    })?;
    std::fs::create_dir(&exact).map_err(|error| {
        std::io::Error::new(error.kind(), format!("create replacement seal: {error}"))
    })?;
    std::fs::write(exact.join("executable"), b"replacement").map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("write replacement executable: {error}"),
        )
    })?;
    std::fs::set_permissions(
        exact.join("executable"),
        std::fs::Permissions::from_mode(0o500),
    )?;
    std::fs::set_permissions(&exact, std::fs::Permissions::from_mode(0o500))?;
    std::mem::forget(authority);

    let result = recover_orphaned_process_directories(&File::open(case.root())?, &records);

    assert!(result.is_err());
    assert!(exact.is_dir());
    assert!(original.is_dir());
    assert!(
        records
            .iter()
            .filter(|record| record.kind() != ProcessOwnedDirectoryKind::SealedExecutable)
            .all(|record| case.root().join(record.name()).is_dir())
    );
    Ok(())
}

#[test]
fn production_authority_rejects_sealed_broker_namespace_substitution()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("broker-substitution")?;
    let authority = case.install_authority()?;
    let broker_name = authority
        .owned_directories()
        .into_iter()
        .find(|record| record.kind() == ProcessOwnedDirectoryKind::SealedBroker)
        .map(|record| record.name().to_os_string())
        .ok_or_else(|| std::io::Error::other("missing sealed broker record"))?;
    let broker = case.root().join(&broker_name);
    std::fs::set_permissions(&broker, std::fs::Permissions::from_mode(0o700))?;
    std::fs::rename(&broker, case.path.join("held-broker"))?;
    std::fs::create_dir(&broker)?;
    std::fs::write(broker.join("executable"), b"#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(
        broker.join("executable"),
        std::fs::Permissions::from_mode(0o500),
    )?;
    std::fs::set_permissions(&broker, std::fs::Permissions::from_mode(0o500))?;
    let spec = ProcessSpec::new(
        vec![OsString::from("admitted-helper")],
        Duration::from_secs(2),
    )?;
    let supervisor = ProcessSupervisor::new(1)?;

    let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
    drop(gate_authority);
    let result = supervisor.run_authorized(&spec, &authority, &mut gate);

    assert!(result.is_err());
    assert!(!supervisor.has_owned_processes());
    Ok(())
}

#[test]
fn production_broker_preserves_cancellation_and_descendant_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let case = Case::create("cancellation")?;
    let authority = case.install_authority()?;
    let token = CancellationToken::new();
    let cancellation = token.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        cancellation.cancel()
    });
    let spec = ProcessSpec::new(
        vec![
            OsString::from("admitted-helper"),
            OsString::from("-c"),
            OsString::from("trap '' TERM; sleep 30 & wait"),
        ],
        Duration::from_secs(5),
    )?
    .with_cancellation(token)
    .with_term_grace(Duration::from_millis(100));
    let supervisor = ProcessSupervisor::new(1)?;

    let outcome = run_with_gate_decision(
        &supervisor,
        &spec,
        &authority,
        TestGateDecision::ReleaseAuthorized,
    )?;
    assert!(canceller.join().is_ok());

    match outcome {
        AuthorizedProcessOutcome::NotStarted(receipt) => {
            assert_eq!(receipt.reason, ProcessNotStartedReason::Cancelled);
            assert!(receipt.launcher_spawned);
            assert!(receipt.cancellation_observed);
        }
        AuthorizedProcessOutcome::Started(report) => {
            assert_eq!(report.termination, ProcessTermination::Cancelled);
            assert!(report.cleanup_complete, "report: {report:?}");
            assert!(report.term_sent, "report: {report:?}");
        }
        AuthorizedProcessOutcome::Uncertain(receipt) => {
            return Err(std::io::Error::other(format!(
                "cancellation left uncertain ownership: {receipt:?}"
            ))
            .into());
        }
    }
    let staging = process_owned_names(&case.root(), ".orchestrator-staging-")?;
    assert_eq!(staging.len(), 1);
    assert!(
        std::fs::read_dir(case.root().join(&staging[0]))?
            .next()
            .is_none()
    );
    Ok(())
}
