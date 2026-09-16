//! Runtime registry, fallback resolution, and enforced dispatch binding.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use thiserror::Error;

use crate::contract::{
    AttemptOutcome, ContractError, DispatchRequest, ExecutionContext, ExecutionRequest,
    MechanicalTermination, PhaseExecutor, RuntimeCap, RuntimeDescriptor, RuntimeFamily,
};
use crate::event::EventSinkErrorKind;

pub const CLAUDE_RUNTIME: &str = "claude";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionKind {
    Direct,
    Defaulted,
    Fallback,
}

#[derive(Debug, Error)]
pub enum RuntimeRegistryError {
    #[error(transparent)]
    InvalidRuntime(#[from] ContractError),
    #[error("no executor is registered for the default runtime")]
    MissingDefault,
    #[error("executor descriptor does not match its registry binding")]
    DescriptorMismatch,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum DispatchError {
    #[error("request effective runtime does not match the resolved executor")]
    EffectiveRuntimeMismatch,
    #[error("resume session runtime does not match the resolved executor")]
    SessionRuntimeMismatch,
    #[error("execution context identity does not match the request")]
    WorkerIdentityMismatch,
    #[error("execution context has already dispatched an attempt")]
    ContextAlreadyUsed,
    #[error("runtime does not advertise required capability {0:?}")]
    UnsupportedCapability(RuntimeCap),
}

struct RegisteredExecutor {
    executor: Arc<dyn PhaseExecutor>,
    descriptor: Option<RuntimeDescriptor>,
}

/// Executor selected for one requested runtime. The optional descriptor is
/// snapshotted at registration, avoiding stateful capability changes between
/// validation and dispatch while preserving Go's unknown/optional behavior.
#[derive(Clone)]
pub struct ResolvedExecutor {
    executor: Arc<dyn PhaseExecutor>,
    descriptor: Option<RuntimeDescriptor>,
    requested_runtime: RuntimeFamily,
    effective_runtime: RuntimeFamily,
    resolution: ResolutionKind,
}

impl ResolvedExecutor {
    #[must_use]
    pub fn requested_runtime(&self) -> &RuntimeFamily {
        &self.requested_runtime
    }

    #[must_use]
    pub fn effective_runtime(&self) -> &RuntimeFamily {
        &self.effective_runtime
    }

    #[must_use]
    pub const fn resolution(&self) -> ResolutionKind {
        self.resolution
    }

    #[must_use]
    pub const fn fell_back_to_claude(&self) -> bool {
        matches!(self.resolution, ResolutionKind::Fallback)
    }

    #[must_use]
    pub fn descriptor(&self) -> Option<&RuntimeDescriptor> {
        self.descriptor.as_ref()
    }

    /// Dispatches through the panic boundary and converts every terminal
    /// authority/event failure into outcome state without erasing partial work.
    ///
    /// Every admitted attempt emits `worker.spawned`, zero or more
    /// `worker.output` events, and one centrally derived terminal event.
    /// Cancellation or deadline expiry detected before provider invocation
    /// closes the stream with `worker.failed` without invoking the provider.
    pub fn execute(
        &self,
        request: &ExecutionRequest,
        context: &mut ExecutionContext<'_>,
    ) -> Result<AttemptOutcome, DispatchError> {
        if request.runtime() != &self.effective_runtime {
            return Err(DispatchError::EffectiveRuntimeMismatch);
        }
        if request
            .resume_from()
            .is_some_and(|session| !session.can_resume_into(&self.effective_runtime))
        {
            return Err(DispatchError::SessionRuntimeMismatch);
        }
        if !context.identity_matches(request) {
            return Err(DispatchError::WorkerIdentityMismatch);
        }
        if request.resume_from().is_some()
            && self
                .descriptor
                .as_ref()
                .is_some_and(|descriptor| !descriptor.supports(RuntimeCap::SessionResume))
        {
            return Err(DispatchError::UnsupportedCapability(
                RuntimeCap::SessionResume,
            ));
        }
        if !context.begin_dispatch(request.attempt()) {
            return Err(DispatchError::ContextAlreadyUsed);
        }
        context.bind_event_stream(request);
        if let Err(error) = context.start_event_stream() {
            let outcome = AttemptOutcome::incomplete(
                MechanicalTermination::EventDeliveryFailure,
                None,
                context.progress_snapshot(),
                std::time::Duration::ZERO,
            );
            if error.kind() == EventSinkErrorKind::Indeterminate {
                context.finalize_attempt_state();
                return Ok(context.apply_authoritative_failures(outcome));
            }
            return Ok(context.finish_without_provider(outcome));
        }
        if let Some(outcome) = context.preflight_outcome() {
            return Ok(context.finish_without_provider(outcome));
        }

        let bound = DispatchRequest::new(request, &self.requested_runtime, &self.effective_runtime);
        let mut outcome =
            match catch_unwind(AssertUnwindSafe(|| self.executor.execute(bound, context))) {
                Ok(outcome) => outcome,
                Err(_) => context.panic_outcome(),
            };
        outcome = outcome.demote_provider_artifacts();
        context.finalize_attempt_state();
        outcome = outcome.reconcile_progress(context.progress_snapshot());
        outcome = context.qualify_completion_evidence(request, outcome);

        let returned_session = outcome.evidence().session().is_some();
        let returned_cost = outcome.evidence().cost().is_some();
        let returned_tools = !outcome.evidence().tool_observations().is_empty();
        let returned_artifacts = !outcome.evidence().artifact_receipts().is_empty();
        let observed_streaming = context.observed_streaming();

        if !outcome.returned_session_matches(&self.effective_runtime) {
            outcome = outcome.reject_provider_session();
        }

        if let Some(descriptor) = self.descriptor.as_ref() {
            for (used, cap) in [
                (returned_session, RuntimeCap::SessionResume),
                (returned_cost, RuntimeCap::CostReport),
                (returned_tools, RuntimeCap::ToolUse),
                (returned_artifacts, RuntimeCap::Artifacts),
                (observed_streaming, RuntimeCap::Streaming),
            ] {
                if used && !descriptor.supports(cap) {
                    outcome = outcome.reject_capability(cap);
                }
            }
        }

        // This is the completion linearization point. Cancellation or deadline
        // observed before it selects a failed terminal; a cancellation racing
        // after admission does not retroactively contradict a committed event.
        context.finalize_attempt_state();
        outcome = context.apply_authoritative_failures(outcome);
        let _ = context.emit_terminal(&outcome);
        Ok(context.apply_authoritative_failures(outcome))
    }
}

impl fmt::Debug for ResolvedExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedExecutor")
            .field("requested_runtime", &self.requested_runtime)
            .field("effective_runtime", &self.effective_runtime)
            .field("resolution", &self.resolution)
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub struct ExecutorRegistry {
    map: BTreeMap<RuntimeFamily, RegisteredExecutor>,
}

impl ExecutorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a backend and snapshots its optional descriptor exactly once.
    pub fn register(
        &mut self,
        runtime: &str,
        executor: Arc<dyn PhaseExecutor>,
    ) -> Result<Option<Arc<dyn PhaseExecutor>>, RuntimeRegistryError> {
        let runtime = RuntimeFamily::parse(runtime)?;
        let descriptor = executor.descriptor();
        if descriptor
            .as_ref()
            .is_some_and(|descriptor| descriptor.runtime() != &runtime)
        {
            return Err(RuntimeRegistryError::DescriptorMismatch);
        }
        Ok(self
            .map
            .insert(
                runtime,
                RegisteredExecutor {
                    executor,
                    descriptor,
                },
            )
            .map(|previous| previous.executor))
    }

    pub fn resolve(
        &self,
        requested_runtime: &str,
    ) -> Result<ResolvedExecutor, RuntimeRegistryError> {
        let (requested_runtime, empty_default) = if requested_runtime.is_empty() {
            (RuntimeFamily::parse(CLAUDE_RUNTIME)?, true)
        } else {
            (RuntimeFamily::parse(requested_runtime)?, false)
        };

        if let Some(registered) = self.map.get(&requested_runtime) {
            return Ok(ResolvedExecutor {
                executor: Arc::clone(&registered.executor),
                descriptor: registered.descriptor.clone(),
                effective_runtime: requested_runtime.clone(),
                requested_runtime,
                resolution: if empty_default {
                    ResolutionKind::Defaulted
                } else {
                    ResolutionKind::Direct
                },
            });
        }

        let effective_runtime = RuntimeFamily::parse(CLAUDE_RUNTIME)?;
        let registered = self
            .map
            .get(&effective_runtime)
            .ok_or(RuntimeRegistryError::MissingDefault)?;
        Ok(ResolvedExecutor {
            executor: Arc::clone(&registered.executor),
            descriptor: registered.descriptor.clone(),
            requested_runtime,
            effective_runtime,
            resolution: ResolutionKind::Fallback,
        })
    }
}
