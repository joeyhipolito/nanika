//! Authenticated daemon adapter for one live durable pilot mission.

use std::io::Write;

use orchestrator_daemon::{
    CanonicalEventOwner, Daemon, DaemonClient, DaemonConfig, DurableCursor,
    MissionCancellationAcknowledgement, OwnerCommit, OwnerCommitFailure, OwnerReplayPage,
    RetryClassification,
};
use orchestrator_process::CancellationToken;

use super::{EXIT_FAILED, EXIT_OK, PilotOptions};

pub(crate) trait CancellationRecorder: Send {
    fn request(&mut self, mission_id: &str) -> MissionCancellationAcknowledgement;
}

struct PilotCancellationOwner {
    recorder: Box<dyn CancellationRecorder>,
    cancellation: CancellationToken,
}

impl CanonicalEventOwner for PilotCancellationOwner {
    fn commit_exact(
        &mut self,
        _event_id: &str,
        _ingress_bytes: &[u8],
    ) -> Result<OwnerCommit, OwnerCommitFailure> {
        Err(unsupported_owner_operation())
    }

    fn replay_page(
        &mut self,
        _after: DurableCursor,
        _max_events: usize,
        _max_bytes: usize,
    ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
        Err(unsupported_owner_operation())
    }

    fn request_mission_cancellation(
        &mut self,
        mission_id: &str,
    ) -> MissionCancellationAcknowledgement {
        let acknowledgement = self.recorder.request(mission_id);
        if matches!(
            acknowledgement,
            MissionCancellationAcknowledgement::NewlyRequested
                | MissionCancellationAcknowledgement::AlreadyRequested
        ) {
            self.cancellation.cancel();
        }
        acknowledgement
    }
}

fn unsupported_owner_operation() -> OwnerCommitFailure {
    OwnerCommitFailure {
        cursor: None,
        retry: RetryClassification::PermanentRejection,
        reason: "pilot daemon accepts cancellation only",
    }
}

pub(crate) fn start(
    options: &PilotOptions,
    recorder: Box<dyn CancellationRecorder>,
    cancellation: CancellationToken,
) -> Result<Daemon, String> {
    // The recorder retains the same writer lease as mission execution until
    // the daemon and every request handler have stopped.
    Daemon::start_embedded_with_exclusive_owner(
        DaemonConfig {
            root: options.output_dir.join("control"),
            port: 0,
            api_key: None,
            cf_team: None,
            cf_aud: None,
            allowed_email: None,
        },
        Box::new(PilotCancellationOwner {
            recorder,
            cancellation,
        }),
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn run_cli(
    options: &PilotOptions,
    output: &mut dyn Write,
    error_output: &mut dyn Write,
) -> u8 {
    let mission_id = options.mission_id.as_deref().unwrap_or_default();
    let acknowledgement = DaemonClient::open(&options.output_dir.join("control"))
        .and_then(|client| client.cancel_mission(mission_id));
    match acknowledgement {
        Ok(MissionCancellationAcknowledgement::NewlyRequested) => {
            let _ = writeln!(output, "cancellation-requested mission={mission_id}");
            EXIT_OK
        }
        Ok(MissionCancellationAcknowledgement::AlreadyRequested) => {
            let _ = writeln!(
                output,
                "cancellation-already-requested mission={mission_id}"
            );
            EXIT_OK
        }
        Ok(MissionCancellationAcknowledgement::UnknownMission) => {
            let _ = writeln!(
                error_output,
                "pilot refused: unknown mission {mission_id:?}"
            );
            EXIT_FAILED
        }
        Ok(MissionCancellationAcknowledgement::Unsupported) => {
            let _ = writeln!(
                error_output,
                "pilot refused: live owner does not support cancellation"
            );
            EXIT_FAILED
        }
        Ok(MissionCancellationAcknowledgement::Rejected) => {
            let _ = writeln!(
                error_output,
                "pilot refused: cancellation was not durably recorded"
            );
            EXIT_FAILED
        }
        Err(error) => {
            let _ = writeln!(
                error_output,
                "pilot refused: no authenticated live durable owner: {error}"
            );
            EXIT_FAILED
        }
    }
}
