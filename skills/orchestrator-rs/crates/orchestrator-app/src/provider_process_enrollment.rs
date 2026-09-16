#[cfg(all(unix, feature = "verification-process-canary"))]
use std::os::unix::ffi::OsStrExt;
use std::{
    fmt,
    fs::File,
    io,
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use orchestrator_exec::{
    ProcessPurpose, ProcessRequest, ProcessRequestFingerprint, ServiceContractError,
};
#[cfg(all(unix, feature = "verification-process-canary"))]
use orchestrator_process::{AuthorizedProcessOutcome, ProcessTermination};
use orchestrator_process::{
    ExecutableFileAttestation, MAX_ATTESTED_EXECUTABLE_BYTES, ProcessError, ProcessSupervisor,
    ProductionProcessLaunchAuthority,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ExecutableCapability, FixtureAuthorityError, WorkspaceError,
    capability::SharedCapabilityRoot,
    fs_util::open_file_nofollow,
    runtime_store::ProcessEffectBinding,
    workspace::{PhaseWorkerAuthority, PhaseWorkerBinding},
};
#[cfg(all(unix, feature = "verification-process-canary"))]
use crate::{
    hermetic_canary_protocol::HermeticCanaryChallenge,
    runtime_store::{AuthorizedLauncherIdentity, ProcessAttemptBinding},
};

const FIXTURE_OUTPUT_BYTES: usize = 1024 * 1024;
const FIXTURE_MODE: &str = "durable-canary";
#[cfg(all(unix, feature = "verification-process-canary"))]
const CANARY_WORKER_ARG: &str = "--hermetic-canary-worker";
#[cfg(all(unix, feature = "verification-process-canary"))]
const CANARY_SENTINEL_NAME: &str = "sentinel.txt";
#[cfg(all(unix, feature = "verification-process-canary"))]
const CANARY_SENTINEL_CONTENT: &[u8] = b"nanika-hermetic-canary-sentinel-v1\n";
const HASH_BUFFER_BYTES: usize = 64 * 1024;
const FIXTURE_EXECUTABLE_ID_PREFIX: &str = "nanika-fixture-v1-";

#[derive(Debug, Error)]
pub(crate) enum ProviderLaunchError {
    #[error("provider process launch admission is invalid")]
    InvalidAdmission,
    #[error("provider process executable admission failed")]
    ExecutableAdmission,
    #[error("provider process request is invalid")]
    InvalidRequest,
    #[error("provider process workspace admission failed")]
    WorkspaceAdmission,
}

impl From<ServiceContractError> for ProviderLaunchError {
    fn from(_error: ServiceContractError) -> Self {
        Self::InvalidRequest
    }
}

impl From<FixtureAuthorityError> for ProviderLaunchError {
    fn from(_error: FixtureAuthorityError) -> Self {
        Self::ExecutableAdmission
    }
}

impl From<WorkspaceError> for ProviderLaunchError {
    fn from(_error: WorkspaceError) -> Self {
        Self::WorkspaceAdmission
    }
}

#[cfg(all(unix, feature = "verification-process-canary"))]
impl From<crate::hermetic_process_canary::HermeticProcessCanaryError> for ProviderLaunchError {
    fn from(_error: crate::hermetic_process_canary::HermeticProcessCanaryError) -> Self {
        Self::ExecutableAdmission
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RetainedFileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    owner: u32,
    links: u64,
}

impl RetainedFileIdentity {
    pub(crate) fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o7777 != 0o700
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
            || metadata.len() == 0
            || metadata.len() > MAX_ATTESTED_EXECUTABLE_BYTES
        {
            return Err(io::Error::other(
                "provider executable has invalid retained metadata",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            links: metadata.nlink(),
        })
    }

    pub(crate) fn matches(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.length == other.length
            && self.mode == other.mode
            && self.owner == other.owner
            && self.links == other.links
    }

    /// Returns the attested byte length, used when constructing an
    /// [`ExecutableFileAttestation`] from a retained identity.
    #[must_use]
    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) const fn length(self) -> u64 {
        self.length
    }
}

enum RetainedExecutableNamespace {
    Fixture {
        boundary: SharedCapabilityRoot,
        relative: PathBuf,
    },
    /// Staged for the hermetic provider dispatch cell; constructed only when
    /// the `verification-process-canary` feature is wired up to dispatch.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[allow(dead_code)]
    HermeticCanary {
        boundary: SharedCapabilityRoot,
        relative: PathBuf,
    },
}

pub(crate) struct RetainedProviderExecutable {
    file: File,
    identity: RetainedFileIdentity,
    attestation: ExecutableFileAttestation,
    logical_id: String,
    namespace: RetainedExecutableNamespace,
}

impl fmt::Debug for RetainedProviderExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedProviderExecutable")
            .field("kind", &"exact-provider-executable")
            .finish_non_exhaustive()
    }
}

impl RetainedProviderExecutable {
    /// Returns the logical executable id that dispatch uses to route a
    /// provider worker request to this retained helper.
    #[must_use]
    #[cfg(all(test, unix, feature = "verification-process-canary"))]
    pub(crate) fn logical_id(&self) -> &str {
        &self.logical_id
    }

    /// Returns the length+SHA-256 attestation recorded at admission.
    #[must_use]
    #[cfg(all(test, unix, feature = "verification-process-canary"))]
    pub(crate) const fn attestation(&self) -> ExecutableFileAttestation {
        self.attestation
    }

    /// Admits a fixture-installed helper as a launch-capable executable.
    fn from_fixture(executable: &ExecutableCapability) -> Result<Self, ProviderLaunchError> {
        let (file, exact_image) = executable.open_verified()?;
        let identity = RetainedFileIdentity::read(&file)
            .map_err(|_| ProviderLaunchError::ExecutableAdmission)?;
        let mut digest = Sha256::new();
        digest.update(exact_image.as_ref());
        let sha256: [u8; 32] = digest.finalize().into();
        let logical_id = format!(
            "{FIXTURE_EXECUTABLE_ID_PREFIX}{}-{}",
            executable.label,
            lowercase_hex(&sha256)
        );
        let retained = Self {
            file,
            identity,
            attestation: ExecutableFileAttestation::new(identity.length, sha256),
            logical_id,
            namespace: RetainedExecutableNamespace::Fixture {
                boundary: Arc::clone(&executable.boundary),
                relative: Path::new("bin").join(&executable.label),
            },
        };
        retained
            .verify()
            .map_err(|_| ProviderLaunchError::ExecutableAdmission)?;
        Ok(retained)
    }

    /// Admits a hermetic process canary's enrolled helper as a launch-capable
    /// executable. The canary's closed enrollment already attested the helper
    /// bytes; this constructor reuses the shared [`RetainedFileIdentity`]
    /// model, pins the helper beneath the fixed compatibility target, and tags the
    /// logical id with the canary prefix so dispatch can distinguish fixture
    /// helpers from disposable-canary helpers.
    ///
    /// Staged for the hermetic provider dispatch cell; currently exercised
    /// only by the launch-integration tests.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[allow(dead_code)]
    pub(crate) fn from_hermetic_canary(
        canary: &crate::hermetic_process_canary::HermeticProcessCanary,
    ) -> Result<Self, ProviderLaunchError> {
        let (file, exact_image) = canary.open_verified()?;
        let identity = RetainedFileIdentity::read(&file)
            .map_err(|_| ProviderLaunchError::ExecutableAdmission)?;
        let mut digest = Sha256::new();
        digest.update(exact_image.as_ref());
        let sha256: [u8; 32] = digest.finalize().into();
        let logical_id = canary.executable_logical_id();
        let compatibility_home: Arc<crate::ProductionBoundary> = Arc::clone(canary.boundary());
        let boundary: SharedCapabilityRoot = compatibility_home;
        let retained = Self {
            file,
            identity,
            attestation: ExecutableFileAttestation::new(identity.length, sha256),
            logical_id,
            namespace: RetainedExecutableNamespace::HermeticCanary {
                boundary,
                relative: Path::new("bin").join(canary.label()),
            },
        };
        retained
            .verify()
            .map_err(|_| ProviderLaunchError::ExecutableAdmission)?;
        Ok(retained)
    }

    /// Reopens the retained helper through its namespace boundary and proves
    /// identity, mode, owner, length, and SHA-256 are unchanged. Exposed
    /// crate-private so the launch machinery and its tests can reverify a
    /// retained executable between dispatch and release.
    pub(crate) fn verify(&self) -> io::Result<()> {
        let current = RetainedFileIdentity::read(&self.file)?;
        if !self.identity.matches(current)
            || hash_exact_file(&self.file, self.identity)? != self.attestation.sha256()
        {
            return Err(io::Error::other(
                "provider executable identity or content changed",
            ));
        }
        let (boundary, relative) = match &self.namespace {
            RetainedExecutableNamespace::Fixture { boundary, relative } => (boundary, relative),
            #[cfg(all(unix, feature = "verification-process-canary"))]
            RetainedExecutableNamespace::HermeticCanary { boundary, relative } => {
                (boundary, relative)
            }
        };
        boundary
            .verify()
            .map_err(|_| io::Error::other("provider capability root changed"))?;
        let mapped = open_file_nofollow(boundary.directory(), relative)?.into_std();
        let mapped_identity = RetainedFileIdentity::read(&mapped)?;
        if !self.identity.matches(mapped_identity)
            || hash_exact_file(&mapped, mapped_identity)? != self.attestation.sha256()
        {
            return Err(io::Error::other("provider executable mapping changed"));
        }
        Ok(())
    }

    fn initialize_launch(
        &self,
        root: File,
        cwd: File,
        cwd_relative: PathBuf,
    ) -> Result<ProductionProcessLaunchAuthority, ProcessError> {
        self.verify().map_err(ProcessError::Spawn)?;
        let executable = self.file.try_clone().map_err(ProcessError::Spawn)?;
        let relative = match &self.namespace {
            RetainedExecutableNamespace::Fixture { relative, .. } => relative,
            #[cfg(all(unix, feature = "verification-process-canary"))]
            RetainedExecutableNamespace::HermeticCanary { relative, .. } => relative,
        };
        ProductionProcessLaunchAuthority::new_disposable_canary_from_attested_file(
            root,
            executable,
            relative.clone(),
            cwd,
            cwd_relative,
            self.attestation,
        )
    }
}

pub(crate) fn hash_exact_file(file: &File, expected: RetainedFileIdentity) -> io::Result<[u8; 32]> {
    let before = RetainedFileIdentity::read(file)?;
    if !expected.matches(before) {
        return Err(io::Error::other(
            "provider executable changed before hashing",
        ));
    }
    let mut digest = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    while offset < expected.length {
        let remaining = expected.length.saturating_sub(offset);
        let bounded = usize::try_from(remaining.min(HASH_BUFFER_BYTES as u64))
            .map_err(|_| io::Error::other("provider executable length is invalid"))?;
        let read = file.read_at(&mut buffer[..bounded], offset)?;
        if read == 0 {
            return Err(io::Error::other(
                "provider executable ended before its admitted length",
            ));
        }
        digest.update(&buffer[..read]);
        offset = offset.saturating_add(read as u64);
    }
    let after = RetainedFileIdentity::read(file)?;
    if !expected.matches(after) {
        return Err(io::Error::other(
            "provider executable changed while hashing",
        ));
    }
    Ok(digest.finalize().into())
}

fn lowercase_hex(bytes: &[u8; 32]) -> String {
    use fmt::Write;

    let mut rendered = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ProviderAdmissionFailure {
    Denied,
    OutsideRoot,
    NotEnrolled,
}

pub(super) struct ExactProviderAdmission {
    purpose: ProcessPurpose,
    executable_id: String,
    working_root: PathBuf,
    request_fingerprint: ProcessRequestFingerprint,
    executable_attestation: ExecutableFileAttestation,
    worker_id: String,
}

impl fmt::Debug for ExactProviderAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactProviderAdmission")
            .field("kind", &"exact-provider-request")
            .finish_non_exhaustive()
    }
}

impl ExactProviderAdmission {
    fn new(
        request: &ProcessRequest,
        executable: &RetainedProviderExecutable,
        worker: &PhaseWorkerBinding,
    ) -> Result<Self, ProviderLaunchError> {
        if request.purpose() != ProcessPurpose::ProviderWorker
            || request.executable_id() != executable.logical_id
            || request.working_root() != worker.canonical_path()
        {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        Ok(Self {
            purpose: ProcessPurpose::ProviderWorker,
            executable_id: executable.logical_id.clone(),
            working_root: worker.canonical_path().to_path_buf(),
            request_fingerprint: request.fingerprint(),
            executable_attestation: executable.attestation,
            worker_id: worker.worker_id().to_string(),
        })
    }

    fn for_application(
        request: &ProcessRequest,
        executable_attestation: ExecutableFileAttestation,
    ) -> Result<Self, ProviderLaunchError> {
        if !matches!(
            request.purpose(),
            ProcessPurpose::ProviderWorker | ProcessPurpose::Verification
        ) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        Ok(Self {
            purpose: request.purpose(),
            executable_id: request.executable_id().to_owned(),
            working_root: request.working_root().to_path_buf(),
            request_fingerprint: request.fingerprint(),
            executable_attestation,
            worker_id: "rust-pilot-process".to_owned(),
        })
    }

    #[cfg(test)]
    fn for_test(request: &ProcessRequest) -> Self {
        Self {
            purpose: request.purpose(),
            executable_id: request.executable_id().to_owned(),
            working_root: request.working_root().to_path_buf(),
            request_fingerprint: request.fingerprint(),
            executable_attestation: ExecutableFileAttestation::new(1, [0_u8; 32]),
            worker_id: "test-worker".to_owned(),
        }
    }

    pub(super) fn admit(&self, request: &ProcessRequest) -> Result<(), ProviderAdmissionFailure> {
        if request.purpose() != self.purpose {
            return Err(ProviderAdmissionFailure::Denied);
        }
        if request.executable_id() != self.executable_id {
            return Err(ProviderAdmissionFailure::NotEnrolled);
        }
        if request.working_root() != self.working_root {
            return Err(ProviderAdmissionFailure::OutsideRoot);
        }
        if request.fingerprint() != self.request_fingerprint {
            return Err(ProviderAdmissionFailure::Denied);
        }
        Ok(())
    }
}

pub(crate) struct PreparedProviderLaunch {
    request: ProcessRequest,
    admission: ExactProviderAdmission,
    executable: RetainedProviderExecutable,
    worker: PhaseWorkerBinding,
    #[cfg(all(unix, feature = "verification-process-canary"))]
    completion: ProviderCompletionAuthority,
}

impl fmt::Debug for PreparedProviderLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProviderLaunch")
            .field("kind", &"unmaterialized-exact-provider-launch")
            .finish_non_exhaustive()
    }
}

impl PreparedProviderLaunch {
    pub(crate) fn from_attested_fixture(
        worker: PhaseWorkerBinding,
        executable: &ExecutableCapability,
    ) -> Result<Self, ProviderLaunchError> {
        if !worker.shares_boundary(&executable.boundary) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        let executable = RetainedProviderExecutable::from_fixture(executable)?;
        let request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            &executable.logical_id,
            worker.canonical_path(),
        )?
        .with_argument(FIXTURE_MODE)?
        .with_max_output_bytes(FIXTURE_OUTPUT_BYTES)?;
        let admission = ExactProviderAdmission::new(&request, &executable, &worker)?;
        Ok(Self {
            request,
            admission,
            executable,
            worker,
            #[cfg(all(unix, feature = "verification-process-canary"))]
            completion: ProviderCompletionAuthority::Fixture,
        })
    }

    /// Composes a hermetic process canary with a worker bounded by the same
    /// fixed compatibility target, producing a prepared disposable-canary
    /// launch. A worker under any other root is
    /// rejected before the helper is admitted.
    ///
    /// Staged for the CLI opt-in wiring; currently exercised only by the
    /// dispatch-integration tests.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[allow(dead_code)]
    pub(crate) fn from_attested_canary(
        worker: PhaseWorkerBinding,
        canary: &crate::hermetic_process_canary::HermeticProcessCanary,
    ) -> Result<Self, ProviderLaunchError> {
        let compatibility_home: Arc<crate::ProductionBoundary> = Arc::clone(canary.boundary());
        let boundary: SharedCapabilityRoot = compatibility_home;
        if !worker.shares_boundary(&boundary) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        let executable = RetainedProviderExecutable::from_hermetic_canary(canary)?;
        let challenge = HermeticCanaryChallenge::for_launch_binding(
            &executable.logical_id,
            executable.attestation.length(),
            &executable.attestation.sha256(),
            worker.worker_id().as_str(),
            worker.canonical_path().as_os_str().as_bytes(),
        );
        let challenge_frame = challenge.request_frame();

        let request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            &executable.logical_id,
            worker.canonical_path(),
        )?
        .with_argument(CANARY_WORKER_ARG)?
        .with_stdin(challenge_frame)?
        .with_max_output_bytes(FIXTURE_OUTPUT_BYTES)?;
        let admission = ExactProviderAdmission::new(&request, &executable, &worker)?;
        Ok(Self {
            request,
            admission,
            executable,
            worker,
            completion: ProviderCompletionAuthority::HermeticCanary(CanaryCompletionAuthority {
                challenge,
            }),
        })
    }

    pub(crate) fn exact_request(&self) -> &ProcessRequest {
        &self.request
    }

    pub(crate) fn worker_id(&self) -> &str {
        &self.admission.worker_id
    }

    pub(crate) fn verify_pre_admission(&self) -> Result<(), ProviderLaunchError> {
        self.admission
            .admit(&self.request)
            .map_err(|_| ProviderLaunchError::InvalidAdmission)?;
        let boundary = match &self.executable.namespace {
            RetainedExecutableNamespace::Fixture { boundary, .. } => boundary,
            #[cfg(all(unix, feature = "verification-process-canary"))]
            RetainedExecutableNamespace::HermeticCanary { boundary, .. } => boundary,
        };
        if !self.worker.shares_boundary(boundary)
            || self.worker.worker_id().as_str() != self.admission.worker_id
            || self.executable.attestation != self.admission.executable_attestation
        {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        #[cfg(all(unix, feature = "verification-process-canary"))]
        if !matches!(
            (&self.executable.namespace, &self.completion),
            (
                RetainedExecutableNamespace::Fixture { .. },
                ProviderCompletionAuthority::Fixture,
            ) | (
                RetainedExecutableNamespace::HermeticCanary { .. },
                ProviderCompletionAuthority::HermeticCanary(_),
            )
        ) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        self.worker.verify()?;
        self.executable
            .verify()
            .map_err(|_| ProviderLaunchError::ExecutableAdmission)
    }

    pub(crate) fn enroll(
        self,
        binding: ProcessEffectBinding,
    ) -> Result<EnrolledProviderLaunch, ProviderLaunchError> {
        if !binding.matches(&self.request) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        self.verify_pre_admission()?;
        let worker = self.worker.materialize()?;
        if worker.worker_id().as_str() != self.admission.worker_id
            || self.executable.attestation != self.admission.executable_attestation
        {
            return Err(ProviderLaunchError::InvalidAdmission);
        }

        let runtime = DeferredProviderRuntime {
            kind: DeferredProviderRuntimeKind::Enrolled(Box::new(EnrolledProviderRuntimeProof {
                worker,
                executable: self.executable,
                #[cfg(all(unix, feature = "verification-process-canary"))]
                completion: self.completion,
            })),
        };
        Ok(EnrolledProviderLaunch {
            request: self.request,
            admission: self.admission,
            runtime,
            binding,
        })
    }

    #[cfg(test)]
    pub(crate) fn enroll_with_runtime_for_test(
        self,
        binding: ProcessEffectBinding,
        runtime: DeferredProviderRuntime,
    ) -> Result<EnrolledProviderLaunch, ProviderLaunchError> {
        if !binding.matches(&self.request) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        Ok(EnrolledProviderLaunch {
            request: self.request,
            admission: self.admission,
            runtime,
            binding,
        })
    }

    #[cfg(test)]
    pub(crate) fn into_exact_request_for_test(self) -> ProcessRequest {
        self.request
    }
}

pub(crate) struct EnrolledProviderLaunch {
    pub(super) request: ProcessRequest,
    pub(super) admission: ExactProviderAdmission,
    pub(super) runtime: DeferredProviderRuntime,
    pub(super) binding: ProcessEffectBinding,
}

impl fmt::Debug for EnrolledProviderLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrolledProviderLaunch")
            .field("kind", &"closed-provider-launch")
            .finish_non_exhaustive()
    }
}

impl EnrolledProviderLaunch {
    pub(crate) fn from_application(
        request: ProcessRequest,
        binding: ProcessEffectBinding,
        executable_attestation: ExecutableFileAttestation,
        verify_preclaim: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync>,
        initialize: Box<
            dyn FnOnce() -> Result<
                    (ProductionProcessLaunchAuthority, ProcessSupervisor),
                    ProcessError,
                > + Send,
        >,
        verify_release: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync>,
    ) -> Result<Self, ProviderLaunchError> {
        if !binding.matches(&request) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        let admission = ExactProviderAdmission::for_application(&request, executable_attestation)?;
        admission
            .admit(&request)
            .map_err(|_| ProviderLaunchError::InvalidAdmission)?;
        Ok(Self {
            request,
            admission,
            runtime: DeferredProviderRuntime {
                kind: DeferredProviderRuntimeKind::Application {
                    verify_preclaim,
                    initialize,
                    verify_release,
                },
            },
            binding,
        })
    }

    #[cfg(test)]
    pub(crate) fn injected_for_test(
        request: &ProcessRequest,
        binding: ProcessEffectBinding,
        runtime: DeferredProviderRuntime,
    ) -> Result<Self, ProviderLaunchError> {
        let request = copy_process_request(request)?;
        if !binding.matches(&request) {
            return Err(ProviderLaunchError::InvalidAdmission);
        }
        Ok(Self {
            admission: ExactProviderAdmission::for_test(&request),
            request,
            runtime,
            binding,
        })
    }

    #[cfg(test)]
    pub(super) fn mismatched_for_actor_test(
        admitted_request: &ProcessRequest,
        actor_request: &ProcessRequest,
        binding: ProcessEffectBinding,
        runtime: DeferredProviderRuntime,
    ) -> Result<Self, ProviderLaunchError> {
        Ok(Self {
            request: copy_process_request(actor_request)?,
            admission: ExactProviderAdmission::for_test(admitted_request),
            runtime,
            binding,
        })
    }

    #[cfg(test)]
    pub(crate) fn initialize_enrolled_runtime_for_test(
        self,
    ) -> Result<EnrolledProviderRuntimeProbe, ProcessError> {
        if self.admission.admit(&self.request).is_err()
            || !self.binding.matches(&self.request)
            || !matches!(&self.runtime.kind, DeferredProviderRuntimeKind::Enrolled(_))
        {
            return Err(ProcessError::InvalidSpec);
        }
        self.runtime.verify_preclaim()?;
        Ok(EnrolledProviderRuntimeProbe {
            initialized: self.runtime.initialize()?,
        })
    }
}

#[cfg(test)]
pub(crate) struct EnrolledProviderRuntimeProbe {
    initialized: InitializedProviderRuntime,
}

#[cfg(test)]
impl EnrolledProviderRuntimeProbe {
    pub(crate) fn verify_for_release(&self) -> Result<(), ProcessError> {
        self.initialized.verify_for_release()
    }
}

pub(crate) struct DeferredProviderRuntime {
    kind: DeferredProviderRuntimeKind,
}

impl fmt::Debug for DeferredProviderRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeferredProviderRuntime")
            .field("kind", &"deferred-provider-runtime")
            .finish_non_exhaustive()
    }
}

enum DeferredProviderRuntimeKind {
    Enrolled(Box<EnrolledProviderRuntimeProof>),
    Application {
        verify_preclaim: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync>,
        initialize: Box<
            dyn FnOnce() -> Result<
                    (ProductionProcessLaunchAuthority, ProcessSupervisor),
                    ProcessError,
                > + Send,
        >,
        verify_release: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync>,
    },
    #[cfg(test)]
    Injected {
        verify_preclaim: Box<dyn Fn() -> Result<(), ProcessError> + Send>,
        initialize: Box<
            dyn FnOnce() -> Result<
                    (ProductionProcessLaunchAuthority, ProcessSupervisor),
                    ProcessError,
                > + Send,
        >,
        verify_release: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync>,
    },
}

struct EnrolledProviderRuntimeProof {
    worker: PhaseWorkerAuthority,
    executable: RetainedProviderExecutable,
    #[cfg(all(unix, feature = "verification-process-canary"))]
    completion: ProviderCompletionAuthority,
}

#[cfg(all(unix, feature = "verification-process-canary"))]
enum ProviderCompletionAuthority {
    Fixture,
    HermeticCanary(CanaryCompletionAuthority),
    HermeticCanaryInFlight,
}

#[cfg(all(unix, feature = "verification-process-canary"))]
pub(super) enum ProviderCompletionDispatch {
    Fixture,
    HermeticCanary(CanaryCompletionAuthority),
    Application,
}

#[cfg(all(unix, feature = "verification-process-canary"))]
fn take_provider_completion(
    executable_is_canary: bool,
    completion: &mut ProviderCompletionAuthority,
) -> Result<ProviderCompletionDispatch, ProcessError> {
    let retained = std::mem::replace(
        completion,
        ProviderCompletionAuthority::HermeticCanaryInFlight,
    );
    match (executable_is_canary, retained) {
        (false, ProviderCompletionAuthority::Fixture) => {
            *completion = ProviderCompletionAuthority::Fixture;
            Ok(ProviderCompletionDispatch::Fixture)
        }
        (true, ProviderCompletionAuthority::HermeticCanary(completion)) => {
            Ok(ProviderCompletionDispatch::HermeticCanary(completion))
        }
        _ => Err(ProcessError::InvalidSpec),
    }
}

#[cfg(all(unix, feature = "verification-process-canary"))]
fn prepared_completion_matches_executable(proof: &EnrolledProviderRuntimeProof) -> bool {
    matches!(
        (&proof.executable.namespace, &proof.completion),
        (
            RetainedExecutableNamespace::Fixture { .. },
            ProviderCompletionAuthority::Fixture,
        ) | (
            RetainedExecutableNamespace::HermeticCanary { .. },
            ProviderCompletionAuthority::HermeticCanary(_),
        )
    )
}

#[cfg(all(unix, feature = "verification-process-canary"))]
fn released_completion_matches_executable(proof: &EnrolledProviderRuntimeProof) -> bool {
    matches!(
        (&proof.executable.namespace, &proof.completion),
        (
            RetainedExecutableNamespace::Fixture { .. },
            ProviderCompletionAuthority::Fixture,
        ) | (
            RetainedExecutableNamespace::HermeticCanary { .. },
            ProviderCompletionAuthority::HermeticCanaryInFlight,
        )
    )
}

#[cfg(all(unix, feature = "verification-process-canary"))]
pub(super) struct CanaryCompletionAuthority {
    challenge: HermeticCanaryChallenge,
}

#[cfg(all(unix, feature = "verification-process-canary"))]
impl CanaryCompletionAuthority {
    fn complete_if_successful(
        self,
        worker: &PhaseWorkerAuthority,
        outcome: &AuthorizedProcessOutcome,
        authorized_identity: Option<&AuthorizedLauncherIdentity>,
        attempt_binding: &ProcessAttemptBinding,
    ) -> Result<(), ProcessError> {
        if !self.successful_proof_is_exact(outcome, authorized_identity, attempt_binding)? {
            return Ok(());
        }
        worker
            .publish_canary_sentinel(CANARY_SENTINEL_NAME, CANARY_SENTINEL_CONTENT)
            .map_err(workspace_process_error)
    }

    fn successful_proof_is_exact(
        &self,
        outcome: &AuthorizedProcessOutcome,
        authorized_identity: Option<&AuthorizedLauncherIdentity>,
        attempt_binding: &ProcessAttemptBinding,
    ) -> Result<bool, ProcessError> {
        let AuthorizedProcessOutcome::Started(report) = outcome else {
            return Ok(false);
        };
        if report.termination != ProcessTermination::Exited(0) {
            return Ok(false);
        }
        if !report.cleanup_complete
            || !report.group_absent
            || !report.direct_child_reaped
            || !report.spawned
        {
            return Ok(false);
        }

        let Some(pid) = report.pid else {
            return Err(ProcessError::InvalidSpec);
        };
        let Some(pgid) = report.pgid else {
            return Err(ProcessError::InvalidSpec);
        };
        let Some(kernel_identity) = report.kernel_identity.as_ref() else {
            return Err(ProcessError::InvalidSpec);
        };
        let Some(authorized_identity) = authorized_identity else {
            return Err(ProcessError::InvalidSpec);
        };
        if kernel_identity.pid() != pid
            || kernel_identity.process_group_id() != pgid
            || !authorized_identity.matches(pid, pgid, kernel_identity.process_start_identity())
            || !authorized_identity.matches_attempt_binding(attempt_binding)
            || report.truncated
            || report.stdout_discarded_bytes != 0
            || report.stderr_discarded_bytes != 0
            || !report.stderr.is_empty()
            || !report.infrastructure_failures.is_empty()
            || report.cancellation_observed
            || report.deadline_observed
            || report.stall_observed
            || report.term_sent
            || report.kill_sent
            || report.escalated_to_kill
            || report.stdout != self.challenge.expected_proof(pid)
        {
            return Err(ProcessError::InvalidSpec);
        }
        Ok(true)
    }
}

impl DeferredProviderRuntime {
    pub(crate) fn verify_preclaim(&self) -> Result<(), ProcessError> {
        match &self.kind {
            DeferredProviderRuntimeKind::Enrolled(proof) => {
                #[cfg(all(unix, feature = "verification-process-canary"))]
                if !prepared_completion_matches_executable(proof) {
                    return Err(ProcessError::InvalidSpec);
                }
                proof
                    .worker
                    .verify_process_cwd()
                    .map_err(workspace_process_error)?;
                proof.executable.verify().map_err(ProcessError::Spawn)
            }
            DeferredProviderRuntimeKind::Application {
                verify_preclaim, ..
            } => verify_preclaim(),
            #[cfg(test)]
            DeferredProviderRuntimeKind::Injected {
                verify_preclaim, ..
            } => verify_preclaim(),
        }
    }

    pub(super) fn initialize(self) -> Result<InitializedProviderRuntime, ProcessError> {
        match self.kind {
            DeferredProviderRuntimeKind::Enrolled(proof) => {
                let (root, cwd, cwd_relative) = proof
                    .worker
                    .process_launch_descriptors()
                    .map_err(workspace_process_error)?;
                let launch = proof
                    .executable
                    .initialize_launch(root, cwd, cwd_relative)?;
                let supervisor = ProcessSupervisor::process_wide()?;
                Ok(InitializedProviderRuntime {
                    launch,
                    supervisor,
                    proof: InitializedProviderProof::Enrolled(proof),
                })
            }
            DeferredProviderRuntimeKind::Application {
                initialize,
                verify_release,
                ..
            } => {
                let (launch, supervisor) = initialize()?;
                Ok(InitializedProviderRuntime {
                    launch,
                    supervisor,
                    proof: InitializedProviderProof::Application { verify_release },
                })
            }
            #[cfg(test)]
            DeferredProviderRuntimeKind::Injected {
                initialize,
                verify_release,
                ..
            } => {
                let (launch, supervisor) = initialize()?;
                Ok(InitializedProviderRuntime {
                    launch,
                    supervisor,
                    proof: InitializedProviderProof::Injected { verify_release },
                })
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn injected(
        initialize: impl FnOnce() -> Result<
            (ProductionProcessLaunchAuthority, ProcessSupervisor),
            ProcessError,
        > + Send
        + 'static,
    ) -> Self {
        Self {
            kind: DeferredProviderRuntimeKind::Injected {
                verify_preclaim: Box::new(|| Ok(())),
                initialize: Box::new(initialize),
                verify_release: Arc::new(|| Ok(())),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn injected_with_verifiers(
        verify_preclaim: impl Fn() -> Result<(), ProcessError> + Send + 'static,
        initialize: impl FnOnce() -> Result<
            (ProductionProcessLaunchAuthority, ProcessSupervisor),
            ProcessError,
        > + Send
        + 'static,
        verify_release: impl Fn() -> Result<(), ProcessError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind: DeferredProviderRuntimeKind::Injected {
                verify_preclaim: Box::new(verify_preclaim),
                initialize: Box::new(initialize),
                verify_release: Arc::new(verify_release),
            },
        }
    }
}

pub(super) struct InitializedProviderRuntime {
    launch: ProductionProcessLaunchAuthority,
    supervisor: ProcessSupervisor,
    proof: InitializedProviderProof,
}

impl fmt::Debug for InitializedProviderRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InitializedProviderRuntime")
            .field("kind", &"initialized-provider-runtime")
            .finish_non_exhaustive()
    }
}

enum InitializedProviderProof {
    Enrolled(Box<EnrolledProviderRuntimeProof>),
    Application {
        verify_release: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync>,
    },
    #[cfg(test)]
    Injected {
        verify_release: Arc<dyn Fn() -> Result<(), ProcessError> + Send + Sync>,
    },
}

impl InitializedProviderRuntime {
    pub(crate) fn launch(&self) -> &ProductionProcessLaunchAuthority {
        &self.launch
    }

    pub(crate) fn supervisor(&self) -> &ProcessSupervisor {
        &self.supervisor
    }

    pub(crate) fn verify_for_release(&self) -> Result<(), ProcessError> {
        match &self.proof {
            InitializedProviderProof::Enrolled(proof) => {
                #[cfg(all(unix, feature = "verification-process-canary"))]
                if !released_completion_matches_executable(proof) {
                    return Err(ProcessError::InvalidSpec);
                }
                proof
                    .worker
                    .verify_process_cwd()
                    .map_err(workspace_process_error)?;
                proof.executable.verify().map_err(ProcessError::Spawn)
            }
            InitializedProviderProof::Application { verify_release } => verify_release(),
            #[cfg(test)]
            InitializedProviderProof::Injected { verify_release } => verify_release(),
        }
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) fn take_provider_completion(
        &mut self,
    ) -> Result<ProviderCompletionDispatch, ProcessError> {
        match &mut self.proof {
            InitializedProviderProof::Enrolled(proof) => {
                let executable_is_canary = matches!(
                    &proof.executable.namespace,
                    RetainedExecutableNamespace::HermeticCanary { .. }
                );
                take_provider_completion(executable_is_canary, &mut proof.completion)
            }
            InitializedProviderProof::Application { .. } => {
                Ok(ProviderCompletionDispatch::Application)
            }
            #[cfg(test)]
            InitializedProviderProof::Injected { .. } => Ok(ProviderCompletionDispatch::Fixture),
        }
    }

    #[cfg(all(unix, feature = "verification-process-canary"))]
    pub(crate) fn complete_canary_if_successful(
        &self,
        completion: ProviderCompletionDispatch,
        outcome: &AuthorizedProcessOutcome,
        authorized_identity: Option<&AuthorizedLauncherIdentity>,
        attempt_binding: &ProcessAttemptBinding,
    ) -> Result<(), ProcessError> {
        match (&self.proof, completion) {
            (
                InitializedProviderProof::Application { .. },
                ProviderCompletionDispatch::Application,
            ) => Ok(()),
            (InitializedProviderProof::Enrolled(proof), ProviderCompletionDispatch::Fixture)
                if matches!(
                    (&proof.executable.namespace, &proof.completion),
                    (
                        RetainedExecutableNamespace::Fixture { .. },
                        ProviderCompletionAuthority::Fixture,
                    )
                ) =>
            {
                Ok(())
            }
            (
                InitializedProviderProof::Enrolled(proof),
                ProviderCompletionDispatch::HermeticCanary(completion),
            ) if released_completion_matches_executable(proof) => completion
                .complete_if_successful(
                    &proof.worker,
                    outcome,
                    authorized_identity,
                    attempt_binding,
                ),
            #[cfg(test)]
            (InitializedProviderProof::Injected { .. }, ProviderCompletionDispatch::Fixture) => {
                Ok(())
            }
            _ => Err(ProcessError::InvalidSpec),
        }
    }
}

fn workspace_process_error(_error: WorkspaceError) -> ProcessError {
    ProcessError::Spawn(io::Error::other(
        "phase worker authority verification failed",
    ))
}

#[cfg(test)]
fn copy_process_request(request: &ProcessRequest) -> Result<ProcessRequest, ServiceContractError> {
    let mut copy = ProcessRequest::new(
        request.purpose(),
        request.executable_id(),
        request.working_root(),
    )?;
    for argument in request.expose_arguments() {
        copy = copy.with_argument(argument)?;
    }
    for (key, value) in request.expose_environment() {
        copy = copy.with_environment(key, value)?;
    }
    if let Some(stdin) = request.expose_stdin() {
        copy = copy.with_stdin(stdin.to_vec())?;
    }
    copy = copy.with_max_output_bytes(request.max_output_bytes())?;
    if request.truncated_output_acknowledged() {
        copy = copy.with_truncated_output_acknowledged();
    }
    Ok(copy)
}

#[cfg(all(test, unix, feature = "verification-process-canary"))]
mod canary_completion_tests {
    use std::time::Duration;

    use orchestrator_process::{KernelProcessIdentity, ProcessReport};
    use rustix::process::getpgid;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn kernel_identity() -> Result<KernelProcessIdentity, Box<dyn std::error::Error>> {
        let pgid = u32::try_from(getpgid(None)?.as_raw_pid())?;
        Ok(KernelProcessIdentity::observe(std::process::id(), pgid)?)
    }

    fn request_fingerprint() -> Result<ProcessRequestFingerprint, ServiceContractError> {
        Ok(ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "canary-test-helper",
            "/canary-test-worker",
        )?
        .fingerprint())
    }

    fn authorized_binding(
        identity: &KernelProcessIdentity,
    ) -> Result<(AuthorizedLauncherIdentity, ProcessAttemptBinding), Box<dyn std::error::Error>>
    {
        let fingerprint = request_fingerprint()?;
        let binding = ProcessAttemptBinding::for_test(fingerprint, 1)?;
        let mut authorized =
            AuthorizedLauncherIdentity::from_kernel_identity(identity, fingerprint);
        authorized.bind_for_test(&binding);
        Ok((authorized, binding))
    }

    fn report(
        challenge: &HermeticCanaryChallenge,
        identity: KernelProcessIdentity,
        termination: ProcessTermination,
    ) -> ProcessReport {
        let pid = identity.pid();
        let pgid = identity.process_group_id();
        ProcessReport {
            termination,
            stdout: challenge.expected_proof(pid),
            stderr: Vec::new(),
            truncated: false,
            stdout_discarded_bytes: 0,
            stderr_discarded_bytes: 0,
            elapsed: Duration::from_millis(1),
            spawned: true,
            pid: Some(pid),
            pgid: Some(pgid),
            kernel_identity: Some(identity),
            cancellation_observed: false,
            deadline_observed: false,
            stall_observed: false,
            term_sent: false,
            kill_sent: false,
            escalated_to_kill: false,
            direct_child_reaped: true,
            group_absent: true,
            cleanup_complete: true,
            infrastructure_failures: Vec::new(),
        }
    }

    #[test]
    fn exact_pid_bound_proof_is_accepted() -> TestResult {
        let challenge = HermeticCanaryChallenge::from_bytes([0x31; 32]);
        let identity = kernel_identity()?;
        let (authorized, binding) = authorized_binding(&identity)?;
        let outcome = AuthorizedProcessOutcome::Started(report(
            &challenge,
            identity,
            ProcessTermination::Exited(0),
        ));
        let completion = CanaryCompletionAuthority { challenge };

        assert!(completion.successful_proof_is_exact(&outcome, Some(&authorized), &binding,)?);
        Ok(())
    }

    #[test]
    fn exact_proof_is_rejected_for_a_different_attempt_binding() -> TestResult {
        let challenge = HermeticCanaryChallenge::from_bytes([0x32; 32]);
        let identity = kernel_identity()?;
        let (authorized, _authorized_binding) = authorized_binding(&identity)?;
        let other_fingerprint = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "different-canary-helper",
            "/canary-test-worker",
        )?
        .fingerprint();
        let other_binding = ProcessAttemptBinding::for_test(other_fingerprint, 1)?;
        let outcome = AuthorizedProcessOutcome::Started(report(
            &challenge,
            identity,
            ProcessTermination::Exited(0),
        ));
        let completion = CanaryCompletionAuthority { challenge };

        assert!(
            completion
                .successful_proof_is_exact(&outcome, Some(&authorized), &other_binding)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn completion_kind_drift_and_double_take_are_rejected() {
        let mut missing_canary = ProviderCompletionAuthority::Fixture;
        assert!(take_provider_completion(true, &mut missing_canary).is_err());

        let mut unexpected_canary =
            ProviderCompletionAuthority::HermeticCanary(CanaryCompletionAuthority {
                challenge: HermeticCanaryChallenge::from_bytes([0x33; 32]),
            });
        assert!(take_provider_completion(false, &mut unexpected_canary).is_err());

        let mut exact_canary =
            ProviderCompletionAuthority::HermeticCanary(CanaryCompletionAuthority {
                challenge: HermeticCanaryChallenge::from_bytes([0x34; 32]),
            });
        assert!(matches!(
            take_provider_completion(true, &mut exact_canary),
            Ok(ProviderCompletionDispatch::HermeticCanary(_))
        ));
        assert!(take_provider_completion(true, &mut exact_canary).is_err());
    }

    #[test]
    fn wrong_pid_or_proof_is_rejected() -> TestResult {
        for corruption in ["pid", "proof"] {
            let challenge = HermeticCanaryChallenge::from_bytes([0x42; 32]);
            let identity = kernel_identity()?;
            let (authorized, binding) = authorized_binding(&identity)?;
            let mut report = report(&challenge, identity, ProcessTermination::Exited(0));
            if corruption == "pid" {
                report.pid = report.pid.map(|pid| pid.saturating_add(1));
            } else {
                report.stdout[0] ^= 0xff;
            }
            let outcome = AuthorizedProcessOutcome::Started(report);
            let completion = CanaryCompletionAuthority { challenge };

            assert!(
                completion
                    .successful_proof_is_exact(&outcome, Some(&authorized), &binding)
                    .is_err(),
                "{corruption}"
            );
        }
        Ok(())
    }

    #[test]
    fn successful_exit_with_stderr_or_discarded_output_is_rejected() -> TestResult {
        for corruption in ["stderr", "truncated", "discarded"] {
            let challenge = HermeticCanaryChallenge::from_bytes([0x53; 32]);
            let identity = kernel_identity()?;
            let (authorized, binding) = authorized_binding(&identity)?;
            let mut report = report(&challenge, identity, ProcessTermination::Exited(0));
            match corruption {
                "stderr" => report.stderr.extend_from_slice(b"unexpected"),
                "truncated" => report.truncated = true,
                "discarded" => report.stdout_discarded_bytes = 1,
                _ => unreachable!(),
            }
            let outcome = AuthorizedProcessOutcome::Started(report);
            let completion = CanaryCompletionAuthority { challenge };

            assert!(
                completion
                    .successful_proof_is_exact(&outcome, Some(&authorized), &binding)
                    .is_err(),
                "{corruption}"
            );
        }
        Ok(())
    }

    #[test]
    fn failure_cancellation_timeout_or_unresolved_ownership_is_not_completed() -> TestResult {
        for termination in [
            ProcessTermination::Exited(7),
            ProcessTermination::Cancelled,
            ProcessTermination::Timeout,
            ProcessTermination::UnresolvedOwnership,
        ] {
            let challenge = HermeticCanaryChallenge::from_bytes([0x64; 32]);
            let identity = kernel_identity()?;
            let (authorized, binding) = authorized_binding(&identity)?;
            let mut report = report(&challenge, identity, termination);
            if termination == ProcessTermination::Cancelled {
                report.cancellation_observed = true;
            } else if termination == ProcessTermination::Timeout {
                report.deadline_observed = true;
            } else if termination == ProcessTermination::UnresolvedOwnership {
                report.cleanup_complete = false;
                report.group_absent = false;
            }
            let outcome = AuthorizedProcessOutcome::Started(report);
            let completion = CanaryCompletionAuthority { challenge };

            assert!(
                !completion.successful_proof_is_exact(&outcome, Some(&authorized), &binding,)?,
                "{termination:?}"
            );
        }
        Ok(())
    }
}
