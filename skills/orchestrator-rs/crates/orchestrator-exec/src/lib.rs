//! Validated executor contract for runtime-specific phase execution.
//!
//! The registry binds requested and effective runtime identities before an
//! executor can run. Provider sessions cannot cross runtime families, partial
//! work survives every mechanical termination, and event persistence failure
//! becomes typed infrastructure state. Time, cancellation, watchdog policy,
//! operation-performing process/effect services are all injected.

mod authority;
mod contract;
mod event;
mod event_log;
mod registry;

pub use authority::{
    AuthorityIntentError, BoundProcessPreflight, Cancellation, Clock, EffectAdmission,
    EffectBudget, EffectKind, EffectReceipt, EffectRequest, EffectService, EffectServiceError,
    EffectServiceErrorKind, EffectStatus, EvidenceVerification, EvidenceVerificationRequest,
    EvidenceVerifier, EvidenceVerifierError, EvidenceVerifierErrorKind, ProcessBudget,
    ProcessExitStatus, ProcessIntent, ProcessPreflight, ProcessPreflightReason, ProcessPurpose,
    ProcessReceipt, ProcessRequest, ProcessRequestFingerprint, ProcessService, ProcessServiceError,
    ProcessServiceErrorKind, ProcessTerminationReceipt, ServiceContractError, WatchdogDecision,
    WatchdogPolicy,
};
pub use contract::{
    ArtifactReceipt, AttemptEvidence, AttemptOutcome, CompletedAttempt, ContractError, CostInfo,
    DispatchRequest, Effort, ExecutionContext, ExecutionRequest, ExecutionRequestDraft,
    ExecutionRequestFingerprint, Failure, FailureKind, IncompleteAttempt, MechanicalTermination,
    PartialWork, PhaseExecutor, RuntimeCap, RuntimeCaps, RuntimeDescriptor, RuntimeFamily,
    SessionHandle, ToolObservation,
};
pub use event::{
    EventReceipt, EventSink, EventSinkError, EventSinkErrorKind, WorkerCompleted,
    WorkerEventCodecError, WorkerEventDraft, WorkerEventEnvelope, WorkerEventError,
    WorkerEventKind, WorkerEventPayload, WorkerFailed, WorkerFailedFields, WorkerIdentity,
    WorkerOutput, WorkerOutputFields, WorkerOutputKind, WorkerSpawned, WorkerSpawnedFields,
};
pub use event_log::{
    EventLogIoError, MissionLogSummary, MissionSnap, PhaseSnap, ReplayRecord, ReplayRecords,
    TailRead, TailRecord, TailRecords, last_sequence, last_sequence_in_bytes, list_mission_logs,
    project_from_log, project_mission_in_bytes, replay, replay_reader, replay_records, tail_chunk,
    tail_reader, tail_since,
};
pub use registry::{
    DispatchError, ExecutorRegistry, ResolutionKind, ResolvedExecutor, RuntimeRegistryError,
};
