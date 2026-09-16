//! Lossless, bounded representations of committed Go worker events.
//!
//! Executors supply only [`WorkerEventPayload`]. [`crate::ExecutionContext`]
//! attaches its own worker identity and an [`EventSink`] assigns durable event
//! ID, UTC timestamp, and sequence metadata. The JSON codec mirrors the
//! committed Go `event.Event` envelope. Rust-authored events persist their
//! non-zero attempt ordinal inside the Go-compatible `data` object; legacy Go
//! events without that field remain readable.

use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use thiserror::Error;

use orchestrator_core::GO_EVENT_JSON_CONTENT_MAX_BYTES;

use crate::contract::{Effort, RuntimeFamily};

const MAX_EVENT_ID_BYTES: usize = 256;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_MODEL_BYTES: usize = 256;
const MAX_PERSONA_BYTES: usize = 256;
const MAX_DIRECTORY_BYTES: usize = 4 * 1024;
const MAX_CHUNK_BYTES: usize = 256 * 1024;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_DURATION_BYTES: usize = 64;
const MAX_ERROR_BYTES: usize = 16 * 1024;
const MAX_STDERR_BYTES: usize = 16 * 1024;

#[derive(Clone, Eq, PartialEq)]
pub struct WorkerIdentity {
    mission_id: String,
    phase_id: String,
    worker_id: String,
}

impl WorkerIdentity {
    pub fn new(
        mission_id: impl Into<String>,
        phase_id: impl Into<String>,
        worker_id: impl Into<String>,
    ) -> Result<Self, WorkerEventError> {
        let mission_id = mission_id.into();
        let phase_id = phase_id.into();
        let worker_id = worker_id.into();
        validate_identifier("mission_id", &mission_id)?;
        validate_identifier("phase_id", &phase_id)?;
        validate_identifier("worker_id", &worker_id)?;
        Ok(Self {
            mission_id,
            phase_id,
            worker_id,
        })
    }

    fn from_persisted(
        mission_id: String,
        phase_id: String,
        worker_id: String,
    ) -> Result<Self, WorkerEventError> {
        validate_identifier("mission_id", &mission_id)?;
        validate_legacy_identifier("phase_id", &phase_id)?;
        validate_legacy_identifier("worker_id", &worker_id)?;
        Ok(Self {
            mission_id,
            phase_id,
            worker_id,
        })
    }

    #[must_use]
    pub fn mission_id(&self) -> &str {
        &self.mission_id
    }

    #[must_use]
    pub fn phase_id(&self) -> &str {
        &self.phase_id
    }

    #[must_use]
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub(crate) fn is_live(&self) -> bool {
        !self.mission_id.is_empty() && !self.phase_id.is_empty() && !self.worker_id.is_empty()
    }
}

impl fmt::Debug for WorkerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerIdentity")
            .field("mission_id_len", &self.mission_id.len())
            .field("phase_id_len", &self.phase_id.len())
            .field("worker_id_len", &self.worker_id.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct WorkerSpawnedFields {
    pub model: String,
    pub runtime: Option<RuntimeFamily>,
    pub effort_level: Option<Effort>,
    pub persona: Option<String>,
    pub directory: String,
}

#[derive(Clone)]
pub struct WorkerSpawned {
    model: String,
    runtime: Option<RuntimeFamily>,
    effort_level: Option<Effort>,
    persona: Option<String>,
    directory: String,
}

impl WorkerSpawned {
    pub fn new(fields: WorkerSpawnedFields) -> Result<Self, WorkerEventError> {
        validate_bounded("model", &fields.model, MAX_MODEL_BYTES)?;
        validate_optional("persona", fields.persona.as_deref(), MAX_PERSONA_BYTES)?;
        validate_required("dir", &fields.directory, MAX_DIRECTORY_BYTES)?;
        Ok(Self {
            model: fields.model,
            runtime: fields.runtime,
            effort_level: fields.effort_level,
            persona: fields.persona,
            directory: fields.directory,
        })
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub fn runtime(&self) -> Option<&RuntimeFamily> {
        self.runtime.as_ref()
    }

    #[must_use]
    pub const fn effort_level(&self) -> Option<Effort> {
        self.effort_level
    }

    #[must_use]
    pub fn persona(&self) -> Option<&str> {
        self.persona.as_deref()
    }

    #[must_use]
    pub fn directory(&self) -> &str {
        &self.directory
    }
}

impl fmt::Debug for WorkerSpawned {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerSpawned")
            .field("has_runtime", &self.runtime.is_some())
            .field("has_effort_level", &self.effort_level.is_some())
            .field("has_persona", &self.persona.is_some())
            .field("model_len", &self.model.len())
            .field("directory", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerOutputKind {
    Text,
    ToolUse,
    ToolResult,
}

impl WorkerOutputKind {
    #[must_use]
    pub const fn as_go_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::ToolUse => "tool_use",
            Self::ToolResult => "tool_result",
        }
    }

    fn parse_go(value: &str) -> Result<Self, WorkerEventCodecError> {
        match value {
            "text" => Ok(Self::Text),
            "tool_use" => Ok(Self::ToolUse),
            "tool_result" => Ok(Self::ToolResult),
            _ => Err(WorkerEventCodecError::InvalidWireValue {
                field: "event_kind",
            }),
        }
    }
}

#[derive(Clone)]
pub struct WorkerOutputFields {
    pub chunk: Option<String>,
    pub event_kind: Option<WorkerOutputKind>,
    pub streaming: Option<bool>,
    pub tool_name: Option<String>,
    pub is_error: Option<bool>,
    pub output_len: Option<usize>,
    pub duration: Option<String>,
}

#[derive(Clone)]
pub struct WorkerOutput {
    chunk: Option<String>,
    event_kind: Option<WorkerOutputKind>,
    streaming: Option<bool>,
    tool_name: Option<String>,
    is_error: Option<bool>,
    output_len: Option<usize>,
    duration: Option<String>,
}

impl WorkerOutput {
    pub fn new(fields: WorkerOutputFields) -> Result<Self, WorkerEventError> {
        if fields.chunk.is_none()
            && fields.event_kind.is_none()
            && fields.streaming.is_none()
            && fields.tool_name.is_none()
            && fields.is_error.is_none()
            && fields.output_len.is_none()
            && fields.duration.is_none()
        {
            return Err(WorkerEventError::EmptyPayload);
        }
        Self::from_persisted(fields)
    }

    /// Decoded history may contain only additive fields from a future writer.
    /// Live construction remains strict through [`Self::new`].
    fn from_persisted(fields: WorkerOutputFields) -> Result<Self, WorkerEventError> {
        validate_optional("chunk", fields.chunk.as_deref(), MAX_CHUNK_BYTES)?;
        validate_optional(
            "tool_name",
            fields.tool_name.as_deref(),
            MAX_TOOL_NAME_BYTES,
        )?;
        validate_optional("duration", fields.duration.as_deref(), MAX_DURATION_BYTES)?;
        Ok(Self {
            chunk: fields.chunk,
            event_kind: fields.event_kind,
            streaming: fields.streaming,
            tool_name: fields.tool_name,
            is_error: fields.is_error,
            output_len: fields.output_len,
            duration: fields.duration,
        })
    }

    #[must_use]
    pub fn chunk(&self) -> Option<&str> {
        self.chunk.as_deref()
    }

    #[must_use]
    pub const fn event_kind(&self) -> Option<WorkerOutputKind> {
        self.event_kind
    }

    #[must_use]
    pub const fn streaming(&self) -> Option<bool> {
        self.streaming
    }

    #[must_use]
    pub fn tool_name(&self) -> Option<&str> {
        self.tool_name.as_deref()
    }

    #[must_use]
    pub const fn is_error(&self) -> Option<bool> {
        self.is_error
    }

    #[must_use]
    pub const fn output_len(&self) -> Option<usize> {
        self.output_len
    }

    #[must_use]
    pub fn duration(&self) -> Option<&str> {
        self.duration.as_deref()
    }
}

impl fmt::Debug for WorkerOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerOutput")
            .field("event_kind", &self.event_kind)
            .field("streaming", &self.streaming)
            .field("has_chunk", &self.chunk.is_some())
            .field("chunk_len", &self.chunk.as_ref().map_or(0, String::len))
            .field("has_tool_name", &self.tool_name.is_some())
            .field("is_error", &self.is_error)
            .field("output_len", &self.output_len)
            .field("has_duration", &self.duration.is_some())
            .finish()
    }
}

#[derive(Clone)]
pub struct WorkerCompleted {
    output_len: usize,
    duration: String,
}

impl WorkerCompleted {
    pub fn new(output_len: usize, duration: impl Into<String>) -> Result<Self, WorkerEventError> {
        let duration = duration.into();
        validate_required("duration", &duration, MAX_DURATION_BYTES)?;
        Ok(Self {
            output_len,
            duration,
        })
    }

    #[must_use]
    pub const fn output_len(&self) -> usize {
        self.output_len
    }

    #[must_use]
    pub fn duration(&self) -> &str {
        &self.duration
    }
}

impl fmt::Debug for WorkerCompleted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerCompleted")
            .field("output_len", &self.output_len)
            .field("duration_len", &self.duration.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct WorkerFailedFields {
    pub error: String,
    pub duration: Option<String>,
    pub output_len: Option<usize>,
    pub exit_code: Option<i32>,
    pub stderr_tail: Option<String>,
}

#[derive(Clone)]
pub struct WorkerFailed {
    error: String,
    duration: Option<String>,
    output_len: Option<usize>,
    exit_code: Option<i32>,
    stderr_tail: Option<String>,
}

impl WorkerFailed {
    pub fn new(fields: WorkerFailedFields) -> Result<Self, WorkerEventError> {
        validate_required("error", &fields.error, MAX_ERROR_BYTES)?;
        validate_optional("duration", fields.duration.as_deref(), MAX_DURATION_BYTES)?;
        validate_optional(
            "stderr_tail",
            fields.stderr_tail.as_deref(),
            MAX_STDERR_BYTES,
        )?;
        Ok(Self {
            error: fields.error,
            duration: fields.duration,
            output_len: fields.output_len,
            exit_code: fields.exit_code,
            stderr_tail: fields.stderr_tail,
        })
    }

    #[must_use]
    pub fn expose_error(&self) -> &str {
        &self.error
    }

    #[must_use]
    pub fn duration(&self) -> Option<&str> {
        self.duration.as_deref()
    }

    #[must_use]
    pub const fn output_len(&self) -> Option<usize> {
        self.output_len
    }

    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    #[must_use]
    pub fn expose_stderr_tail(&self) -> Option<&str> {
        self.stderr_tail.as_deref()
    }
}

impl fmt::Debug for WorkerFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerFailed")
            .field("error", &"[REDACTED]")
            .field("error_len", &self.error.len())
            .field("has_duration", &self.duration.is_some())
            .field("output_len", &self.output_len)
            .field("exit_code", &self.exit_code)
            .field("has_stderr_tail", &self.stderr_tail.is_some())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerEventKind {
    Spawned,
    Output,
    Completed,
    Failed,
}

impl WorkerEventKind {
    #[must_use]
    pub const fn as_go_str(self) -> &'static str {
        match self {
            Self::Spawned => "worker.spawned",
            Self::Output => "worker.output",
            Self::Completed => "worker.completed",
            Self::Failed => "worker.failed",
        }
    }

    fn parse_go(value: &str) -> Result<Self, WorkerEventCodecError> {
        match value {
            "worker.spawned" => Ok(Self::Spawned),
            "worker.output" => Ok(Self::Output),
            "worker.completed" => Ok(Self::Completed),
            "worker.failed" => Ok(Self::Failed),
            _ => Err(WorkerEventCodecError::UnsupportedEventType),
        }
    }
}

#[derive(Clone)]
pub enum WorkerEventPayload {
    Spawned(WorkerSpawned),
    Output(WorkerOutput),
    Completed(WorkerCompleted),
    Failed(WorkerFailed),
}

impl WorkerEventPayload {
    #[must_use]
    pub const fn kind(&self) -> WorkerEventKind {
        match self {
            Self::Spawned(_) => WorkerEventKind::Spawned,
            Self::Output(_) => WorkerEventKind::Output,
            Self::Completed(_) => WorkerEventKind::Completed,
            Self::Failed(_) => WorkerEventKind::Failed,
        }
    }

    #[must_use]
    pub fn requires_streaming_capability(&self) -> bool {
        matches!(self, Self::Output(output) if output.streaming() == Some(true))
    }
}

impl fmt::Debug for WorkerEventPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawned(payload) => formatter.debug_tuple("Spawned").field(payload).finish(),
            Self::Output(payload) => formatter.debug_tuple("Output").field(payload).finish(),
            Self::Completed(payload) => formatter.debug_tuple("Completed").field(payload).finish(),
            Self::Failed(payload) => formatter.debug_tuple("Failed").field(payload).finish(),
        }
    }
}

/// Ephemeral event passed to the sink. Identity cannot be supplied by an
/// executor because construction is crate-private.
#[derive(Clone, Copy)]
pub struct WorkerEventDraft<'a> {
    identity: &'a WorkerIdentity,
    attempt: u32,
    payload: &'a WorkerEventPayload,
}

impl<'a> WorkerEventDraft<'a> {
    pub(crate) const fn new(
        identity: &'a WorkerIdentity,
        attempt: u32,
        payload: &'a WorkerEventPayload,
    ) -> Self {
        Self {
            identity,
            attempt,
            payload,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &'a WorkerIdentity {
        self.identity
    }

    /// Returns the non-zero execution-attempt ordinal bound by the dispatch
    /// boundary. Sinks must reject drafts whose attempt differs from their
    /// own durable attempt authority.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn payload(&self) -> &'a WorkerEventPayload {
        self.payload
    }

    #[must_use]
    pub const fn kind(&self) -> WorkerEventKind {
        self.payload.kind()
    }

    pub fn into_envelope(
        self,
        receipt: EventReceipt,
        attempt: u32,
    ) -> Result<WorkerEventEnvelope, WorkerEventError> {
        if attempt == 0 || attempt != self.attempt {
            return Err(WorkerEventError::InvalidAttempt);
        }
        Ok(WorkerEventEnvelope {
            receipt,
            identity: self.identity.clone(),
            attempt: Some(attempt),
            payload: self.payload.clone(),
            source_json: None,
        })
    }
}

impl fmt::Debug for WorkerEventDraft<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerEventDraft")
            .field("identity", &self.identity)
            .field("attempt", &self.attempt)
            .field("payload", &self.payload)
            .finish()
    }
}

/// Sink-assigned durable envelope metadata.
#[derive(Clone)]
pub struct EventReceipt {
    id: String,
    timestamp: String,
    sequence: i64,
}

impl EventReceipt {
    pub fn new(
        id: impl Into<String>,
        timestamp: impl Into<String>,
        sequence: i64,
    ) -> Result<Self, WorkerEventError> {
        let id = id.into();
        let timestamp = timestamp.into();
        validate_identifier("id", &id)?;
        if !id.starts_with("evt_") {
            return Err(WorkerEventError::InvalidEventId);
        }
        Self::from_persisted(id, timestamp, sequence)
    }

    fn from_persisted(
        id: String,
        timestamp: String,
        sequence: i64,
    ) -> Result<Self, WorkerEventError> {
        validate_identifier("id", &id)?;
        validate_scalar("timestamp", &timestamp, MAX_TIMESTAMP_BYTES)?;
        if !is_rfc3339(&timestamp) {
            return Err(WorkerEventError::InvalidTimestamp);
        }
        if sequence <= 0 {
            return Err(WorkerEventError::InvalidSequence);
        }
        Ok(Self {
            id,
            timestamp,
            sequence,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }

    #[must_use]
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }
}

impl fmt::Debug for EventReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventReceipt")
            .field("id_len", &self.id.len())
            .field("timestamp", &self.timestamp)
            .field("sequence", &self.sequence)
            .finish()
    }
}

/// Persisted Go-compatible event envelope.
#[derive(Clone)]
pub struct WorkerEventEnvelope {
    receipt: EventReceipt,
    identity: WorkerIdentity,
    attempt: Option<u32>,
    payload: WorkerEventPayload,
    source_json: Option<String>,
}

impl WorkerEventEnvelope {
    #[must_use]
    pub fn receipt(&self) -> &EventReceipt {
        &self.receipt
    }

    #[must_use]
    pub fn identity(&self) -> &WorkerIdentity {
        &self.identity
    }

    /// Returns the persisted attempt ordinal. Legacy Go events may omit it.
    #[must_use]
    pub const fn attempt(&self) -> Option<u32> {
        self.attempt
    }

    #[must_use]
    pub fn payload(&self) -> &WorkerEventPayload {
        &self.payload
    }

    #[must_use]
    pub const fn kind(&self) -> WorkerEventKind {
        self.payload.kind()
    }

    /// Returns the exact authoritative JSON supplied to [`Self::from_json`].
    /// Newly constructed live envelopes have no source bytes until persisted.
    #[must_use]
    pub fn preserved_source_json(&self) -> Option<&[u8]> {
        self.source_json.as_deref().map(str::as_bytes)
    }

    pub fn to_json(&self) -> Result<String, WorkerEventCodecError> {
        if let Some(source_json) = &self.source_json {
            return Ok(source_json.clone());
        }
        let wire = WireEnvelope::try_from(self)?;
        Ok(serde_json::to_string(&wire)?)
    }

    pub fn from_json(json: &str) -> Result<Self, WorkerEventCodecError> {
        let content = json.strip_suffix('\n').unwrap_or(json);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if content.len() > GO_EVENT_JSON_CONTENT_MAX_BYTES {
            return Err(WorkerEventCodecError::EventTooLarge {
                max: GO_EVENT_JSON_CONTENT_MAX_BYTES,
            });
        }
        reject_duplicate_object_keys(json)?;
        let wire: WireEnvelope = serde_json::from_str(json)?;
        let mut envelope = Self::try_from(wire)?;
        envelope.source_json = Some(json.to_owned());
        Ok(envelope)
    }
}

impl fmt::Debug for WorkerEventEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerEventEnvelope")
            .field("receipt", &self.receipt)
            .field("identity", &self.identity)
            .field("attempt", &self.attempt)
            .field("payload", &self.payload)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventSinkErrorKind {
    Backpressure,
    Persistence,
    /// The durable boundary was crossed but acknowledgement did not complete.
    /// The owning authority must reconcile before admitting another event.
    Indeterminate,
    Rejected,
    Unavailable,
}

#[derive(Clone, Error)]
#[error("worker event delivery failed ({kind:?})")]
pub struct EventSinkError {
    kind: EventSinkErrorKind,
    detail: String,
}

impl EventSinkError {
    pub fn new(
        kind: EventSinkErrorKind,
        detail: impl Into<String>,
    ) -> Result<Self, WorkerEventError> {
        let detail = detail.into();
        validate_required("event_sink_detail", &detail, MAX_ERROR_BYTES)?;
        Ok(Self { kind, detail })
    }

    #[must_use]
    pub const fn kind(&self) -> EventSinkErrorKind {
        self.kind
    }

    #[must_use]
    pub fn expose_detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn rejected(detail: &'static str) -> Self {
        Self {
            kind: EventSinkErrorKind::Rejected,
            detail: detail.to_owned(),
        }
    }
}

impl fmt::Debug for EventSinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventSinkError")
            .field("kind", &self.kind)
            .field("detail", &"[REDACTED]")
            .field("detail_len", &self.detail.len())
            .finish()
    }
}

/// Persists one context-bound draft and returns its assigned durable metadata.
///
/// `Ok` is the acknowledged commit point. An `Err` other than
/// [`EventSinkErrorKind::Indeterminate`] means no event was durably committed.
/// `Indeterminate` means the durable boundary may have been crossed; the sink's
/// owning authority must close admission and reconcile before another emit.
pub trait EventSink {
    fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError>;
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WorkerEventError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds the {max} byte contract limit")]
    TooLong { field: &'static str, max: usize },
    #[error("{field} contains disallowed control characters")]
    ControlCharacter { field: &'static str },
    #[error("worker.output must contain at least one committed Go payload field")]
    EmptyPayload,
    #[error("event ID does not match the committed Go prefix")]
    InvalidEventId,
    #[error("event timestamp is not valid RFC3339")]
    InvalidTimestamp,
    #[error("durable event sequence must be positive")]
    InvalidSequence,
    #[error("worker event attempt must be positive")]
    InvalidAttempt,
}

#[derive(Debug, Error)]
pub enum WorkerEventCodecError {
    #[error("worker event JSON exceeds the {max} byte contract limit")]
    EventTooLarge { max: usize },
    #[error("invalid worker event JSON")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Contract(#[from] WorkerEventError),
    #[error(transparent)]
    Runtime(#[from] crate::contract::ContractError),
    #[error("unsupported event type in worker-event codec")]
    UnsupportedEventType,
    #[error("worker event is missing its data object")]
    MissingData,
    #[error("invalid committed Go value for {field}")]
    InvalidWireValue { field: &'static str },
}

#[derive(Serialize, Deserialize)]
struct WireEnvelope {
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    timestamp: String,
    sequence: i64,
    mission_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    phase_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    worker_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
struct WireSpawned {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persona: Option<String>,
    #[serde(rename = "dir")]
    directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
}

/// Recursively validates JSON without normalizing it, rejecting any object
/// whose meaning would depend on a parser's duplicate-key policy.
#[derive(Clone, Copy)]
struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueJson::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<UniqueJson>()?.is_some() {}
        Ok(UniqueJson)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<UniqueJson>()?;
        }
        Ok(UniqueJson)
    }
}

fn reject_duplicate_object_keys(json: &str) -> Result<(), serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(json);
    UniqueJson::deserialize(&mut deserializer)?;
    deserializer.end()
}

#[derive(Serialize, Deserialize)]
struct WireOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chunk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct WireCompleted {
    output_len: usize,
    duration: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct WireFailed {
    error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stderr_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt: Option<u32>,
}

impl TryFrom<&WorkerEventEnvelope> for WireEnvelope {
    type Error = WorkerEventCodecError;

    fn try_from(event: &WorkerEventEnvelope) -> Result<Self, Self::Error> {
        let data = match &event.payload {
            WorkerEventPayload::Spawned(payload) => serde_json::to_value(WireSpawned {
                model: payload.model.clone(),
                runtime: payload
                    .runtime
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
                effort_level: payload.effort_level.map(Effort::as_str).map(str::to_owned),
                persona: payload.persona.clone(),
                directory: payload.directory.clone(),
                attempt: event.attempt,
            })?,
            WorkerEventPayload::Output(payload) => serde_json::to_value(WireOutput {
                chunk: payload.chunk.clone(),
                event_kind: payload
                    .event_kind
                    .map(WorkerOutputKind::as_go_str)
                    .map(str::to_owned),
                streaming: payload.streaming,
                tool_name: payload.tool_name.clone(),
                is_error: payload.is_error,
                output_len: payload.output_len,
                duration: payload.duration.clone(),
                attempt: event.attempt,
            })?,
            WorkerEventPayload::Completed(payload) => serde_json::to_value(WireCompleted {
                output_len: payload.output_len,
                duration: payload.duration.clone(),
                attempt: event.attempt,
            })?,
            WorkerEventPayload::Failed(payload) => serde_json::to_value(WireFailed {
                error: payload.error.clone(),
                duration: payload.duration.clone(),
                output_len: payload.output_len,
                exit_code: payload.exit_code,
                stderr_tail: payload.stderr_tail.clone(),
                attempt: event.attempt,
            })?,
        };
        Ok(Self {
            id: event.receipt.id.clone(),
            event_type: event.payload.kind().as_go_str().to_owned(),
            timestamp: event.receipt.timestamp.clone(),
            sequence: event.receipt.sequence,
            mission_id: event.identity.mission_id.clone(),
            phase_id: event.identity.phase_id.clone(),
            worker_id: event.identity.worker_id.clone(),
            data: Some(data),
        })
    }
}

impl TryFrom<WireEnvelope> for WorkerEventEnvelope {
    type Error = WorkerEventCodecError;

    fn try_from(wire: WireEnvelope) -> Result<Self, Self::Error> {
        let kind = WorkerEventKind::parse_go(&wire.event_type)?;
        let data = wire.data.ok_or(WorkerEventCodecError::MissingData)?;
        let (payload, attempt) = match kind {
            WorkerEventKind::Spawned => {
                let raw: WireSpawned = serde_json::from_value(data)?;
                let runtime = raw.runtime.map(RuntimeFamily::parse).transpose()?;
                let effort_level = raw.effort_level.as_deref().map(parse_effort).transpose()?;
                let attempt = validate_wire_attempt(raw.attempt)?;
                (
                    WorkerEventPayload::Spawned(WorkerSpawned::new(WorkerSpawnedFields {
                        model: raw.model,
                        runtime,
                        effort_level,
                        persona: raw.persona,
                        directory: raw.directory,
                    })?),
                    attempt,
                )
            }
            WorkerEventKind::Output => {
                let raw: WireOutput = serde_json::from_value(data)?;
                let event_kind = raw
                    .event_kind
                    .as_deref()
                    .map(WorkerOutputKind::parse_go)
                    .transpose()?;
                let attempt = validate_wire_attempt(raw.attempt)?;
                (
                    WorkerEventPayload::Output(WorkerOutput::from_persisted(WorkerOutputFields {
                        chunk: raw.chunk,
                        event_kind,
                        streaming: raw.streaming,
                        tool_name: raw.tool_name,
                        is_error: raw.is_error,
                        output_len: raw.output_len,
                        duration: raw.duration,
                    })?),
                    attempt,
                )
            }
            WorkerEventKind::Completed => {
                let raw: WireCompleted = serde_json::from_value(data)?;
                let attempt = validate_wire_attempt(raw.attempt)?;
                (
                    WorkerEventPayload::Completed(WorkerCompleted::new(
                        raw.output_len,
                        raw.duration,
                    )?),
                    attempt,
                )
            }
            WorkerEventKind::Failed => {
                let raw: WireFailed = serde_json::from_value(data)?;
                let attempt = validate_wire_attempt(raw.attempt)?;
                (
                    WorkerEventPayload::Failed(WorkerFailed::new(WorkerFailedFields {
                        error: raw.error,
                        duration: raw.duration,
                        output_len: raw.output_len,
                        exit_code: raw.exit_code,
                        stderr_tail: raw.stderr_tail,
                    })?),
                    attempt,
                )
            }
        };
        Ok(Self {
            receipt: EventReceipt::from_persisted(wire.id, wire.timestamp, wire.sequence)?,
            identity: WorkerIdentity::from_persisted(
                wire.mission_id,
                wire.phase_id,
                wire.worker_id,
            )?,
            attempt,
            payload,
            source_json: None,
        })
    }
}

fn validate_wire_attempt(attempt: Option<u32>) -> Result<Option<u32>, WorkerEventCodecError> {
    if attempt == Some(0) {
        return Err(WorkerEventCodecError::InvalidWireValue { field: "attempt" });
    }
    Ok(attempt)
}

fn parse_effort(value: &str) -> Result<Effort, WorkerEventCodecError> {
    match value {
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "xhigh" => Ok(Effort::XHigh),
        _ => Err(WorkerEventCodecError::InvalidWireValue {
            field: "effort_level",
        }),
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), WorkerEventError> {
    if value.is_empty() {
        return Err(WorkerEventError::Empty { field });
    }
    if value.len() > MAX_EVENT_ID_BYTES {
        return Err(WorkerEventError::TooLong {
            field,
            max: MAX_EVENT_ID_BYTES,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(WorkerEventError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_legacy_identifier(field: &'static str, value: &str) -> Result<(), WorkerEventError> {
    if value.is_empty() {
        return Ok(());
    }
    validate_identifier(field, value)
}

// Matches the strict RFC3339 grammar and ranges accepted by the Go event log.
fn is_rfc3339(value: &str) -> bool {
    if value.len() < 20 || !value.is_ascii() {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        std::str::from_utf8(bytes.get(range)?).ok()?.parse().ok()
    };
    let (Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(5..7),
        number(8..10),
        number(11..13),
        number(14..16),
        number(17..19),
    ) else {
        return false;
    };
    let year = number(0..4).unwrap_or(0);
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    if day == 0 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let mut offset = 19;
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let fraction_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == fraction_start {
            return false;
        }
    }
    match bytes.get(offset) {
        Some(b'Z') => offset + 1 == bytes.len(),
        Some(b'+') | Some(b'-') => {
            if offset + 6 != bytes.len() || bytes.get(offset + 3) != Some(&b':') {
                return false;
            }
            let zone_hour = std::str::from_utf8(&bytes[offset + 1..offset + 3])
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            let zone_minute = std::str::from_utf8(&bytes[offset + 4..offset + 6])
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            matches!((zone_hour, zone_minute), (Some(0..=23), Some(0..=59)))
        }
        _ => false,
    }
}

fn validate_required(field: &'static str, value: &str, max: usize) -> Result<(), WorkerEventError> {
    if value.is_empty() {
        return Err(WorkerEventError::Empty { field });
    }
    validate_bounded(field, value, max)
}

fn validate_scalar(field: &'static str, value: &str, max: usize) -> Result<(), WorkerEventError> {
    if value.is_empty() {
        return Err(WorkerEventError::Empty { field });
    }
    if value.len() > max {
        return Err(WorkerEventError::TooLong { field, max });
    }
    if value.chars().any(char::is_control) {
        return Err(WorkerEventError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_optional(
    field: &'static str,
    value: Option<&str>,
    max: usize,
) -> Result<(), WorkerEventError> {
    if let Some(value) = value {
        validate_bounded(field, value, max)?;
    }
    Ok(())
}

fn validate_bounded(field: &'static str, value: &str, max: usize) -> Result<(), WorkerEventError> {
    if value.len() > max {
        return Err(WorkerEventError::TooLong { field, max });
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(WorkerEventError::ControlCharacter { field });
    }
    Ok(())
}
