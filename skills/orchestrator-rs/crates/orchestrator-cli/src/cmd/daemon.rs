use orchestrator_app::{
    CanonicalEventLog, CanonicalEventLogError, FixtureAdmissionPolicy, FreshFixtureAuthority,
    IsolatedFixtureRoot,
};
use orchestrator_core::{MissionId, decode_event_line, encode_current_event};
use orchestrator_daemon::{
    CanonicalEventOwner, CommitFreshness, Daemon, DaemonConfig, DaemonError, DurableCursor,
    OwnedEvent, OwnerCommit, OwnerCommitFailure, OwnerReplayPage, RetryClassification,
    daemon_status, stop_daemon,
};
use std::{fs, io, io::Write, path::Path, sync::Arc};
use thiserror::Error;

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum DaemonCommand {
    Help,
    StartHelp,
    StopHelp,
    StatusHelp,
    Start {
        port: u16,
        api_key: Option<String>,
        cf_team: Option<String>,
        cf_aud: Option<String>,
        allowed_email: Option<String>,
    },
    Stop,
    Status,
    UnsupportedLegacy(&'static str),
}

impl std::fmt::Debug for DaemonCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start {
                port,
                api_key,
                cf_team,
                cf_aud,
                allowed_email,
            } => formatter
                .debug_struct("Start")
                .field("port", port)
                .field("api_key", &api_key.as_ref().map(|_| "[REDACTED]"))
                .field("cf_team", cf_team)
                .field("cf_aud", &cf_aud.as_ref().map(|_| "[REDACTED]"))
                .field(
                    "allowed_email",
                    &allowed_email.as_ref().map(|_| "[REDACTED]"),
                )
                .finish(),
            Self::Help => formatter.write_str("Help"),
            Self::StartHelp => formatter.write_str("StartHelp"),
            Self::StopHelp => formatter.write_str("StopHelp"),
            Self::StatusHelp => formatter.write_str("StatusHelp"),
            Self::Stop => formatter.write_str("Stop"),
            Self::Status => formatter.write_str("Status"),
            Self::UnsupportedLegacy(surface) => formatter
                .debug_tuple("UnsupportedLegacy")
                .field(surface)
                .finish(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonCommandError {
    #[error("daemon start requires a valid --port in 0..=65535")]
    InvalidPort,
    #[error("unknown daemon flag {0:?}")]
    UnknownFlag(String),
    #[error("unknown daemon subcommand {0:?}")]
    UnknownSubcommand(String),
    #[error(transparent)]
    Service(#[from] orchestrator_daemon::DaemonError),
    #[error("cannot enroll the daemon canonical event owner: {0}")]
    Owner(String),
    #[error("legacy daemon surface {0:?} is unsupported by the Rust daemon")]
    UnsupportedLegacy(&'static str),
    #[error("cannot write command output: {0}")]
    Output(#[from] std::io::Error),
}

pub(crate) fn parse(arguments: &[String]) -> Result<DaemonCommand, DaemonCommandError> {
    let Some((subcommand, rest)) = arguments.split_first() else {
        return Ok(DaemonCommand::Help);
    };
    match subcommand.as_str() {
        "--help" | "-h" => Ok(DaemonCommand::Help),
        "stop"
            if rest
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h")) =>
        {
            Ok(DaemonCommand::StopHelp)
        }
        "status"
            if rest
                .iter()
                .any(|value| matches!(value.as_str(), "--help" | "-h")) =>
        {
            Ok(DaemonCommand::StatusHelp)
        }
        "stop" if rest.iter().all(|value| !value.starts_with('-')) => Ok(DaemonCommand::Stop),
        "status" if rest.iter().all(|value| !value.starts_with('-')) => Ok(DaemonCommand::Status),
        "stop" | "status" => Err(DaemonCommandError::UnknownFlag(
            rest.iter()
                .find(|value| value.starts_with('-'))
                .cloned()
                .unwrap_or_default(),
        )),
        "start" => parse_start(rest),
        "backfill-vectors" => Ok(DaemonCommand::UnsupportedLegacy("backfill-vectors")),
        other => Err(DaemonCommandError::UnknownSubcommand(other.to_owned())),
    }
}

pub(crate) const fn is_help(command: &DaemonCommand) -> bool {
    matches!(
        command,
        DaemonCommand::Help
            | DaemonCommand::StartHelp
            | DaemonCommand::StopHelp
            | DaemonCommand::StatusHelp
    )
}

fn parse_start(arguments: &[String]) -> Result<DaemonCommand, DaemonCommandError> {
    if arguments
        .iter()
        .any(|value| matches!(value.as_str(), "--help" | "-h"))
    {
        return Ok(DaemonCommand::StartHelp);
    }
    let mut port = 7331u16;
    let mut api_key = None;
    let mut cf_team = None;
    let mut cf_aud = None;
    let mut allowed_email = None;
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let argument = &arguments[cursor];
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value))
            });
        match name {
            "--port" => {
                let value = take_value(arguments, &mut cursor, inline)?;
                port = value.parse().map_err(|_| DaemonCommandError::InvalidPort)?;
            }
            "--api-key" => api_key = Some(take_value(arguments, &mut cursor, inline)?.to_owned()),
            "--cf-team" => cf_team = Some(take_value(arguments, &mut cursor, inline)?.to_owned()),
            "--cf-aud" => cf_aud = Some(take_value(arguments, &mut cursor, inline)?.to_owned()),
            "--allowed-email" => {
                allowed_email = Some(take_value(arguments, &mut cursor, inline)?.to_owned());
            }
            "--dashboard" => {
                let _ = take_value(arguments, &mut cursor, inline)?;
                return Ok(DaemonCommand::UnsupportedLegacy("--dashboard"));
            }
            other => return Err(DaemonCommandError::UnknownFlag(other.to_owned())),
        }
        cursor += 1;
    }
    Ok(DaemonCommand::Start {
        port,
        api_key,
        cf_team,
        cf_aud,
        allowed_email,
    })
}

fn take_value<'a>(
    arguments: &'a [String],
    cursor: &mut usize,
    inline: Option<&'a str>,
) -> Result<&'a str, DaemonCommandError> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *cursor += 1;
    arguments.get(*cursor).map(String::as_str).ok_or_else(|| {
        let prior = cursor.saturating_sub(1);
        DaemonCommandError::UnknownFlag(arguments[prior].clone())
    })
}

pub(crate) fn run(
    command: DaemonCommand,
    home: &Path,
    output: &mut impl Write,
) -> Result<(), DaemonCommandError> {
    match command {
        DaemonCommand::Help => output.write_all(DAEMON_HELP.as_bytes())?,
        DaemonCommand::StartHelp => output.write_all(DAEMON_START_HELP.as_bytes())?,
        DaemonCommand::StopHelp => output.write_all(DAEMON_STOP_HELP.as_bytes())?,
        DaemonCommand::StatusHelp => output.write_all(DAEMON_STATUS_HELP.as_bytes())?,
        DaemonCommand::UnsupportedLegacy(surface) => {
            return Err(DaemonCommandError::UnsupportedLegacy(surface));
        }
        DaemonCommand::Start {
            port,
            api_key,
            cf_team,
            cf_aud,
            allowed_email,
        } => {
            let owner = daemon_event_owner(home)?;
            let daemon = Daemon::start(
                DaemonConfig {
                    root: home.to_owned(),
                    port,
                    api_key,
                    cf_team,
                    cf_aud,
                    allowed_email,
                },
                owner,
            )?;
            writeln!(
                output,
                "daemon: socket {}",
                home.join("daemon.sock").display()
            )?;
            writeln!(
                output,
                "daemon: events {}",
                home.join("events.sock").display()
            )?;
            writeln!(output, "daemon: api    http://{}", daemon.address())?;
            output.flush()?;
            daemon.wait()?;
        }
        DaemonCommand::Stop => {
            if stop_daemon(home)? {
                writeln!(output, "daemon: stop requested")?;
            } else {
                writeln!(output, "daemon: not running")?;
            }
        }
        DaemonCommand::Status => match daemon_status(home) {
            Err(DaemonError::IdentityMismatch) if legacy_or_unverified_state(home) => writeln!(
                output,
                "daemon: legacy or unverified daemon state present (Rust will not target it)"
            )?,
            Err(error) => return Err(error.into()),
            Ok(Some(identity)) => writeln!(
                output,
                "daemon: running (PID {}, 127.0.0.1:{})",
                identity.pid, identity.port
            )?,
            Ok(None) => writeln!(output, "daemon: not running")?,
        },
    }
    Ok(())
}

fn legacy_or_unverified_state(home: &Path) -> bool {
    home.join("daemon.pid").is_file() && !home.join("daemon.identity.json").exists()
}

/// Recovers the application-owned fixture authority and gives the daemon only
/// its value-only event port. The adapter, not the daemon, retains the sole
/// `CanonicalEventLog` writer and all filesystem capability state.
fn daemon_event_owner(home: &Path) -> Result<Box<dyn CanonicalEventOwner>, DaemonCommandError> {
    let user_home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| DaemonCommandError::Owner("HOME is unavailable".into()))?;
    let checkout = std::env::current_dir().map_err(|error| {
        DaemonCommandError::Owner(format!("cannot identify repository checkout: {error}"))
    })?;
    let temporary = std::env::temp_dir();
    let policy = FixtureAdmissionPolicy::new(user_home, checkout, &temporary);
    let isolated = IsolatedFixtureRoot::identify(home).map_err(daemon_owner_error)?;
    let authority =
        FreshFixtureAuthority::recover(isolated, &policy).map_err(daemon_owner_error)?;
    CanonicalLogOwner::recover(authority, home)
        .map(|owner| Box::new(owner) as Box<dyn CanonicalEventOwner>)
        .map_err(daemon_owner_error)
}

fn daemon_owner_error(error: impl std::fmt::Display) -> DaemonCommandError {
    DaemonCommandError::Owner(error.to_string())
}

/// One mission-scoped canonical writer. A fresh root binds to the first event;
/// a restart binds from the sole recovered canonical log. This keeps daemon
/// cursors identical to the mission's public event sequence without creating a
/// replay or cursor sidecar.
struct CanonicalLogOwner {
    log: Option<CanonicalEventLog>,
    authority: FreshFixtureAuthority,
    mission: Option<MissionId>,
    prior_bytes: Vec<u8>,
    events: Vec<OwnedEvent>,
}

impl CanonicalLogOwner {
    fn recover(
        authority: FreshFixtureAuthority,
        home: &Path,
    ) -> Result<Self, CanonicalEventLogError> {
        let mission =
            sole_canonical_mission(home).map_err(|_| CanonicalEventLogError::InvalidKnownEntry)?;
        let log = mission
            .clone()
            .map(|mission| authority.open_canonical_event_log(mission))
            .transpose()?;
        let (prior_bytes, events) = log
            .as_ref()
            .map(recover_canonical_state)
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            log,
            authority,
            mission,
            prior_bytes,
            events,
        })
    }

    fn bind_or_validate_mission(&mut self, mission: &str) -> Result<MissionId, OwnerCommitFailure> {
        let parsed = MissionId::new(mission).map_err(|_| permanent("invalid mission identity"))?;
        match &self.mission {
            Some(bound) if bound != &parsed => {
                Err(permanent("event mission does not match canonical owner"))
            }
            Some(_) => Ok(parsed),
            None => {
                let log = self
                    .authority
                    .open_canonical_event_log(parsed.clone())
                    .map_err(map_canonical_failure)?;
                self.log = Some(log);
                self.mission = Some(parsed.clone());
                Ok(parsed)
            }
        }
    }

    fn recover_after_indeterminate(
        &mut self,
        event_id: &str,
        ingress_bytes: &[u8],
    ) -> Result<Option<OwnerCommit>, OwnerCommitFailure> {
        self.log.take();
        let mission = self
            .mission
            .clone()
            .ok_or_else(|| retry_same(None, "canonical mission binding was lost"))?;
        let log = self
            .authority
            .open_canonical_event_log(mission)
            .map_err(map_canonical_failure)?;
        let (prior_bytes, events) = recover_canonical_state(&log).map_err(map_canonical_failure)?;
        self.log = Some(log);
        self.prior_bytes = prior_bytes;
        self.events = events;
        match self.events.iter().find(|event| event.event_id == event_id) {
            Some(event) if event.ingress_bytes.as_ref() == ingress_bytes => Ok(Some(OwnerCommit {
                event: event.clone(),
                freshness: CommitFreshness::New,
                retry: RetryClassification::NoRetryRequired,
            })),
            Some(event) => Err(OwnerCommitFailure {
                cursor: Some(event.cursor),
                retry: RetryClassification::PermanentRejection,
                reason: "event id was committed with different exact bytes",
            }),
            None => Ok(None),
        }
    }
}

impl CanonicalEventOwner for CanonicalLogOwner {
    fn commit_exact(
        &mut self,
        event_id: &str,
        ingress_bytes: &[u8],
    ) -> Result<OwnerCommit, OwnerCommitFailure> {
        let decoded = decode_event_line(ingress_bytes)
            .map_err(|_| permanent("canonical ingress is not a valid event line"))?;
        if decoded.record.id != event_id {
            return Err(permanent("event id does not match exact ingress bytes"));
        }
        let canonical = encode_current_event(&decoded.record)
            .map_err(|_| permanent("canonical ingress cannot be encoded"))?;
        if canonical != ingress_bytes {
            return Err(permanent(
                "ingress bytes are not the exact Go-compatible canonical line",
            ));
        }
        if decoded.record.sequence <= 0 {
            return Err(permanent("event cursor must be positive"));
        }
        if let Some(existing) = self.events.iter().find(|event| event.event_id == event_id) {
            if existing.ingress_bytes.as_ref() != ingress_bytes {
                return Err(OwnerCommitFailure {
                    cursor: Some(existing.cursor),
                    retry: RetryClassification::PermanentRejection,
                    reason: "event id was reused with different exact bytes",
                });
            }
            return Ok(OwnerCommit {
                event: existing.clone(),
                freshness: CommitFreshness::AlreadyCommitted,
                retry: RetryClassification::NoRetryRequired,
            });
        }
        let expected_cursor = self
            .events
            .last()
            .map_or(1, |prior| prior.cursor.get().saturating_add(1));
        if decoded.record.sequence != expected_cursor {
            return Err(permanent(
                "event cursor is not the exact monotonic successor",
            ));
        }
        let _mission = self.bind_or_validate_mission(&decoded.record.mission_id)?;
        let cursor = DurableCursor::new(decoded.record.sequence)
            .map_err(|_| permanent("event cursor must be positive"))?;
        let mut next_line = Vec::with_capacity(ingress_bytes.len().saturating_add(2));
        if !self.prior_bytes.is_empty() && !self.prior_bytes.ends_with(b"\n") {
            next_line.push(b'\n');
        }
        next_line.extend_from_slice(ingress_bytes);
        next_line.push(b'\n');
        let publish = self
            .log
            .as_mut()
            .ok_or_else(|| permanent("canonical event log is not bound"))?
            .publish_exact_next_line(&self.prior_bytes, &next_line);
        match publish {
            Ok(projection)
                if projection.event_id() == event_id
                    && projection.sequence() == cursor.get()
                    && projection.line_bytes() == ingress_bytes => {}
            Ok(_) => return Err(permanent("canonical owner committed a divergent receipt")),
            Err(
                CanonicalEventLogError::AppendIndeterminate { .. }
                | CanonicalEventLogError::AppendIdentityIndeterminate
                | CanonicalEventLogError::RecoveryRequired,
            ) => {
                if let Some(commit) = self.recover_after_indeterminate(event_id, ingress_bytes)? {
                    return Ok(commit);
                }
                return Err(retry_same(
                    None,
                    "canonical acknowledgement was lost before commit could be proven",
                ));
            }
            Err(error) => return Err(map_canonical_failure(error)),
        }
        self.prior_bytes.extend_from_slice(&next_line);
        let event = OwnedEvent {
            cursor,
            event_id: event_id.to_owned(),
            ingress_bytes: Arc::from(ingress_bytes),
        };
        self.events.push(event.clone());
        Ok(OwnerCommit {
            event,
            freshness: CommitFreshness::New,
            retry: RetryClassification::NoRetryRequired,
        })
    }

    fn replay_page(
        &mut self,
        after: DurableCursor,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
        let high_water = self
            .events
            .last()
            .map_or(DurableCursor::origin(), |event| event.cursor);
        let mut bytes = 0usize;
        let mut events = Vec::new();
        let mut has_more = false;
        for event in self.events.iter().filter(|event| event.cursor > after) {
            let next_bytes = bytes.saturating_add(event.ingress_bytes.len());
            if events.len() == max_events || (!events.is_empty() && next_bytes > max_bytes) {
                has_more = true;
                break;
            }
            if next_bytes > max_bytes {
                return Err(OwnerCommitFailure {
                    cursor: Some(event.cursor),
                    retry: RetryClassification::PermanentRejection,
                    reason: "canonical replay event exceeds its page byte budget",
                });
            }
            bytes = next_bytes;
            events.push(event.clone());
        }
        Ok(OwnerReplayPage {
            high_water,
            events,
            has_more,
        })
    }
}

fn sole_canonical_mission(home: &Path) -> io::Result<Option<MissionId>> {
    let entries = match fs::read_dir(home.join("events")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut mission = None;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(stem) = Path::new(&name)
            .file_stem()
            .and_then(|value| value.to_str())
        else {
            continue;
        };
        if Path::new(&name)
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let parsed = MissionId::new(stem).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid canonical mission filename",
            )
        })?;
        if mission.replace(parsed).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "daemon composition supports exactly one canonical mission log",
            ));
        }
    }
    Ok(mission)
}

fn recover_canonical_state(
    log: &CanonicalEventLog,
) -> Result<(Vec<u8>, Vec<OwnedEvent>), CanonicalEventLogError> {
    let mut prior_bytes = Vec::new();
    let mut events = Vec::with_capacity(log.events().len());
    let mut prior_cursor = 0i64;
    for decoded in log.events() {
        let content = decoded
            .raw_line
            .strip_suffix(b"\n")
            .unwrap_or(&decoded.raw_line);
        if content.ends_with(b"\r")
            || encode_current_event(&decoded.record)
                .map_err(|_| CanonicalEventLogError::InvalidEvent)?
                .as_slice()
                != content
            || decoded.record.sequence <= prior_cursor
        {
            return Err(CanonicalEventLogError::InvalidEvent);
        }
        let cursor = DurableCursor::new(decoded.record.sequence)
            .map_err(|_| CanonicalEventLogError::InvalidSequence)?;
        events.push(OwnedEvent {
            cursor,
            event_id: decoded.record.id.clone(),
            ingress_bytes: Arc::from(content),
        });
        prior_cursor = decoded.record.sequence;
        prior_bytes.extend_from_slice(&decoded.raw_line);
    }
    Ok((prior_bytes, events))
}

fn permanent(reason: &'static str) -> OwnerCommitFailure {
    OwnerCommitFailure {
        cursor: None,
        retry: RetryClassification::PermanentRejection,
        reason,
    }
}

fn retry_same(cursor: Option<DurableCursor>, reason: &'static str) -> OwnerCommitFailure {
    OwnerCommitFailure {
        cursor,
        retry: RetryClassification::RetrySameEventId,
        reason,
    }
}

fn map_canonical_failure(error: CanonicalEventLogError) -> OwnerCommitFailure {
    match error {
        CanonicalEventLogError::AppendIndeterminate { .. }
        | CanonicalEventLogError::AppendIdentityIndeterminate
        | CanonicalEventLogError::RecoveryRequired => retry_same(
            None,
            "canonical event acknowledgement is indeterminate; retry the same event id",
        ),
        _ => permanent("canonical event owner rejected the event"),
    }
}

pub(crate) const DAEMON_HELP: &str = r#"The daemon relays orchestrator events to external consumers via SSE.

Rust B1 supports authenticated private UDS ingress/events plus loopback
HTTP health, event ingress, and SSE. Legacy dashboard, mission REST, remote
Cloudflare Access, notification, and vector-backfill surfaces are unsupported.

Usage:
  orchestrator daemon [command]

Available Commands:
  start       Start the daemon (runs in foreground)
  status      Show daemon status
  stop        Request authenticated graceful shutdown

Flags:
  -h, --help   help for daemon
"#;

pub(crate) const DAEMON_START_HELP: &str = r#"Start the event relay daemon.

Usage:
  orchestrator daemon start [flags]

Flags:
      --allowed-email string   unsupported with Rust B1 remote authentication
      --api-key string         API key for Bearer token authentication
      --cf-aud string          unsupported with Rust B1 remote authentication
      --cf-team string         unsupported with Rust B1 remote authentication
      --dashboard string       unsupported legacy dashboard surface
  -h, --help                   help for start
      --port int               HTTP API port (default 7331)
"#;

pub(crate) const DAEMON_STOP_HELP: &str = r#"Request authenticated graceful shutdown.

Usage:
  orchestrator daemon stop
"#;

pub(crate) const DAEMON_STATUS_HELP: &str = r#"Show daemon status using recorded PID, process-group, and start identity.

Usage:
  orchestrator daemon status
"#;

#[cfg(test)]
mod tests {
    use super::{DaemonCommand, parse};

    #[test]
    fn start_debug_redacts_every_credential_bearing_value() -> Result<(), super::DaemonCommandError>
    {
        let command = parse(
            &[
                "start",
                "--api-key",
                "api-secret",
                "--cf-team",
                "team.example",
                "--cf-aud",
                "aud-secret",
                "--allowed-email",
                "owner-secret@example.com",
            ]
            .map(str::to_owned),
        )?;
        assert!(matches!(command, DaemonCommand::Start { .. }));
        let debug = format!("{command:?}");
        assert!(debug.contains("[REDACTED]"));
        for forbidden in ["api-secret", "aud-secret", "owner-secret@example.com"] {
            assert!(!debug.contains(forbidden));
        }
        Ok(())
    }
}
