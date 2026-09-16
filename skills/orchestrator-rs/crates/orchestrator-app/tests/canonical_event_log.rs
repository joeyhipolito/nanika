use orchestrator_app::{
    CanonicalEventLogError, CanonicalEventLogFault, FixtureAdmissionPolicy, FreshFixtureAuthority,
    IsolatedFixtureRoot,
};
use orchestrator_core::{
    GO_EVENT_JSON_CONTENT_MAX_BYTES, MissionId, decode_event_line, encode_current_event,
};
use orchestrator_exec::{WorkerEventEnvelope, WorkerEventKind};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

struct FixtureCase {
    parent: PathBuf,
    root: PathBuf,
    authority: FreshFixtureAuthority,
}

impl Drop for FixtureCase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn fixture_case(label: &str) -> TestResult<FixtureCase> {
    let number = CASE.fetch_add(1, Ordering::Relaxed);
    let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = canonical_temp.join(format!(
        "orchestrator-rs-canonical-event-log-{}-{number}-{label}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &canonical_temp);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    Ok(FixtureCase {
        parent,
        root,
        authority,
    })
}

fn mode(path: &Path) -> TestResult<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::symlink_metadata(path)?.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(0)
    }
}

fn event(id: &str, event_type: &str, sequence: i64, mission_id: &str) -> Vec<u8> {
    format!(
        r#"{{"id":"{id}","type":"{event_type}","timestamp":"2026-07-15T00:00:00Z","sequence":{sequence},"mission_id":"{mission_id}"}}"#
    )
    .into_bytes()
}

fn sized_event(id: &str, sequence: i64, mission_id: &str, target_bytes: usize) -> Vec<u8> {
    let prefix = format!(
        r#"{{"id":"{id}","type":"mission.started","timestamp":"2026-07-15T00:00:00Z","sequence":{sequence},"mission_id":"{mission_id}","data":{{"padding":""#
    );
    let suffix = r#""}}"#;
    assert!(target_bytes >= prefix.len() + suffix.len());
    format!(
        "{prefix}{}{suffix}",
        "x".repeat(target_bytes - prefix.len() - suffix.len())
    )
    .into_bytes()
}

#[test]
fn canonical_backend_creates_only_root_events_path_with_private_modes() -> TestResult {
    let case = fixture_case("path")?;
    let mission = MissionId::new("mission-path")?;
    let log = case.authority.open_canonical_event_log(mission)?;

    let events = case.root.join("events");
    let file = events.join("mission-path.jsonl");
    assert!(file.is_file());
    assert_eq!(mode(&events)?, 0o700);
    assert_eq!(mode(&file)?, 0o600);
    assert!(
        !case
            .root
            .join("workspaces/mission-path/events.jsonl")
            .exists()
    );
    assert!(log.events().is_empty());
    Ok(())
}

#[test]
fn canonical_backend_process_lease_releases_when_writer_drops() -> TestResult {
    let case = fixture_case("lease")?;
    let mission = MissionId::new("mission-lease")?;
    let writer = case.authority.open_canonical_event_log(mission.clone())?;
    assert!(matches!(
        case.authority.open_canonical_event_log(mission.clone()),
        Err(CanonicalEventLogError::WriterLeased)
    ));
    drop(writer);
    let reopened = case.authority.open_canonical_event_log(mission)?;
    assert!(reopened.events().is_empty());
    Ok(())
}

#[test]
fn canonical_backend_appends_and_reopens_in_forensic_file_order() -> TestResult {
    let case = fixture_case("order")?;
    let mission = MissionId::new("mission-order")?;
    let first = event(
        "evt_0000000000000001",
        "mission.started",
        1,
        mission.as_str(),
    );
    let second = event("evt_0000000000000002", "phase.started", 2, mission.as_str());
    {
        let mut log = case.authority.open_canonical_event_log(mission.clone())?;
        log.append_current_json(&first)?;
        log.append_current_json(&second)?;
        assert_eq!(
            log.events()
                .iter()
                .map(|event| event.record.id.as_str())
                .collect::<Vec<_>>(),
            ["evt_0000000000000001", "evt_0000000000000002"]
        );
        assert_eq!(log.next_sequence(), Some(3));
    }

    let expected = [first.as_slice(), b"\n", second.as_slice(), b"\n"].concat();
    assert_eq!(
        std::fs::read(case.root.join("events/mission-order.jsonl"))?,
        expected
    );
    let reopened = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(reopened.events().len(), 2);
    assert_eq!(reopened.next_sequence(), Some(3));
    Ok(())
}

#[test]
fn canonical_backend_rejects_same_length_path_replacement_before_append() -> TestResult {
    let case = fixture_case("same-length-path-replacement")?;
    let mission = MissionId::new("mission-same-length-path-replacement")?;
    let first = event(
        "evt_0000000000000013",
        "mission.started",
        1,
        mission.as_str(),
    );
    let second = event("evt_0000000000000014", "phase.started", 2, mission.as_str());
    let mut log = case.authority.open_canonical_event_log(mission)?;
    log.append_current_json(&first)?;
    let path = case
        .root
        .join("events/mission-same-length-path-replacement.jsonl");
    let replacement = case.root.join("events/.same-length-replacement");
    let committed = std::fs::read(&path)?;
    write_private(&replacement, &committed)?;
    std::fs::rename(&replacement, &path)?;

    assert!(matches!(
        log.append_current_json(&second),
        Err(CanonicalEventLogError::InvalidKnownEntry)
    ));
    assert_eq!(std::fs::read(path)?, committed);
    Ok(())
}

#[test]
fn canonical_backend_rejects_same_inode_same_length_prefix_mutation_before_append() -> TestResult {
    let case = fixture_case("same-inode-mutation")?;
    let mission = MissionId::new("mission-same-inode-mutation")?;
    let first = event(
        "evt_0000000000000015",
        "mission.started",
        1,
        mission.as_str(),
    );
    let second = event("evt_0000000000000016", "phase.started", 2, mission.as_str());
    let mut log = case.authority.open_canonical_event_log(mission)?;
    log.append_current_json(&first)?;
    let path = case.root.join("events/mission-same-inode-mutation.jsonl");
    let mut mutated = std::fs::read(&path)?;
    let marker = b"mission.started";
    let marker_start = mutated
        .windows(marker.len())
        .position(|window| window == marker)
        .ok_or("event type marker is missing")?;
    mutated[marker_start + marker.len() - 1] = b'D';
    std::fs::write(&path, &mutated)?;

    assert!(matches!(
        log.append_current_json(&second),
        Err(CanonicalEventLogError::AuthoritativePrefixChanged)
    ));
    assert_eq!(std::fs::read(path)?, mutated);
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonical_backend_rejects_preexisting_hard_link_without_mutating_its_external_alias()
-> TestResult {
    let case = fixture_case("preexisting-hard-link")?;
    let mission = MissionId::new("mission-preexisting-hard-link")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let path = case.root.join("events/mission-preexisting-hard-link.jsonl");
    std::fs::remove_file(&path)?;
    let external = case.parent.join("external-preexisting.jsonl");
    write_private(&external, b"external sentinel")?;
    std::fs::hard_link(&external, &path)?;

    assert!(matches!(
        case.authority.open_canonical_event_log(mission),
        Err(CanonicalEventLogError::InvalidKnownEntry)
    ));
    assert_eq!(std::fs::read(external)?, b"external sentinel");
    Ok(())
}

#[cfg(unix)]
#[test]
fn canonical_backend_rejects_hard_link_added_after_open_before_append() -> TestResult {
    let case = fixture_case("late-hard-link")?;
    let mission = MissionId::new("mission-late-hard-link")?;
    let source = event(
        "evt_0000000000000021",
        "mission.started",
        1,
        mission.as_str(),
    );
    let path = case.root.join("events/mission-late-hard-link.jsonl");
    let external = case.parent.join("external-late.jsonl");
    let mut log = case.authority.open_canonical_event_log(mission)?;
    std::fs::hard_link(&path, &external)?;

    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::InvalidKnownEntry)
    ));
    assert!(std::fs::read(&path)?.is_empty());
    assert!(std::fs::read(external)?.is_empty());
    Ok(())
}

#[test]
fn canonical_backend_preserves_unknown_envelope_and_data_bytes() -> TestResult {
    let case = fixture_case("forward")?;
    let mission = MissionId::new("mission-forward")?;
    let source = br#"{"id":"evt_0000000000000003","type":"future.kind","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-forward","future_envelope":{"keep":true},"data":{"future_data":[1,2,3]}}"#;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let path = case.root.join("events/mission-forward.jsonl");
    std::fs::write(&path, [source.as_slice(), b"\n"].concat())?;

    let reopened = case.authority.open_canonical_event_log(mission)?;
    let decoded = reopened.events().first().ok_or("event is missing")?;
    assert_eq!(decoded.raw_line, [source.as_slice(), b"\n"].concat());
    assert_eq!(
        decoded
            .record
            .extra
            .get("future_envelope")
            .and_then(|value| value.get("keep"))
            .ok_or("forward envelope field is missing")?,
        &serde_json::Value::Bool(true)
    );
    assert_eq!(
        decoded
            .record
            .data
            .as_ref()
            .and_then(|data| data.get("future_data"))
            .ok_or("forward data field is missing")?,
        &serde_json::json!([1, 2, 3])
    );
    Ok(())
}

#[test]
fn canonical_backend_normatively_preserves_crlf_history_without_rewrite() -> TestResult {
    let case = fixture_case("crlf")?;
    let mission = MissionId::new("mission-crlf")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let source = event(
        "evt_0000000000000009",
        "mission.started",
        1,
        mission.as_str(),
    );
    let preserved = [source.as_slice(), b"\r\n"].concat();
    let path = case.root.join("events/mission-crlf.jsonl");
    std::fs::write(&path, &preserved)?;

    let reopened = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(
        reopened.events()[0].raw_line.as_slice(),
        preserved.as_slice()
    );
    assert_eq!(std::fs::read(path)?, preserved);
    Ok(())
}

#[test]
fn canonical_backend_preserves_pretty_existing_history_without_normalizing_it() -> TestResult {
    let case = fixture_case("pretty-history")?;
    let mission = MissionId::new("mission-pretty-history")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let source = b" { \"id\": \"evt_pretty\", \"type\": \"mission.started\", \"timestamp\": \"2026-07-15T00:00:00Z\", \"sequence\": 1, \"mission_id\": \"mission-pretty-history\" }\n";
    let path = case.root.join("events/mission-pretty-history.jsonl");
    std::fs::write(&path, source)?;

    let reopened = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(reopened.events()[0].raw_line.as_slice(), source);
    assert_eq!(std::fs::read(path)?, source);
    Ok(())
}

#[test]
fn canonical_backend_reads_legacy_non_generated_event_id_without_rewrite() -> TestResult {
    let case = fixture_case("legacy-id")?;
    let mission = MissionId::new("mission-legacy-id")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let source = event("evt_first", "mission.started", 1, mission.as_str());
    let preserved = [source.as_slice(), b"\n"].concat();
    let path = case.root.join("events/mission-legacy-id.jsonl");
    std::fs::write(&path, &preserved)?;

    let reopened = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(reopened.events()[0].record.id, "evt_first");
    assert_eq!(std::fs::read(path)?, preserved);
    Ok(())
}

#[test]
fn canonical_backend_accepts_complete_eof_record_and_syncs_separator_with_next_append() -> TestResult
{
    let case = fixture_case("complete-eof")?;
    let mission = MissionId::new("mission-complete-eof")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let first = event(
        "evt_0000000000000010",
        "mission.started",
        1,
        mission.as_str(),
    );
    let second = event("evt_0000000000000011", "phase.started", 2, mission.as_str());
    let path = case.root.join("events/mission-complete-eof.jsonl");
    std::fs::write(&path, &first)?;

    let mut reopened = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(reopened.events()[0].raw_line, first);
    assert_eq!(std::fs::read(&path)?, first);
    reopened.append_current_json(&second)?;

    assert_eq!(
        std::fs::read(path)?,
        [first.as_slice(), b"\n", second.as_slice(), b"\n"].concat()
    );
    assert_eq!(
        reopened.events()[0].raw_line,
        [first, b"\n".to_vec()].concat()
    );
    assert_eq!(
        reopened.events()[1].raw_line,
        [second, b"\n".to_vec()].concat()
    );
    assert_eq!(reopened.next_sequence(), Some(3));
    Ok(())
}

#[test]
fn canonical_backend_rejects_invalid_partial_eof_tail_without_rewrite() -> TestResult {
    let case = fixture_case("partial-eof")?;
    let mission = MissionId::new("mission-partial-eof")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let source = br#"{"id":"evt_0000000000000010","type":"mission.started""#;
    let path = case.root.join("events/mission-partial-eof.jsonl");
    std::fs::write(&path, source)?;

    assert!(matches!(
        case.authority.open_canonical_event_log(mission),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    assert_eq!(std::fs::read(path)?, source);
    Ok(())
}

#[test]
fn canonical_backend_normatively_blocks_structurally_incomplete_sequence_only_history() -> TestResult
{
    let case = fixture_case("sequence-only")?;
    let mission = MissionId::new("mission-sequence-only")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let source = b"{\"sequence\":99}\n";
    let path = case.root.join("events/mission-sequence-only.jsonl");
    std::fs::write(&path, source)?;

    assert!(matches!(
        case.authority.open_canonical_event_log(mission),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    assert_eq!(std::fs::read(path)?, source);
    Ok(())
}

#[test]
fn canonical_backend_rejects_malformed_append_without_writing() -> TestResult {
    let case = fixture_case("malformed")?;
    let mission = MissionId::new("mission-malformed")?;
    let mut log = case.authority.open_canonical_event_log(mission)?;
    assert!(matches!(
        log.append_current_json(br#"{"id":"evt_0000000000000004"}"#),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    assert!(std::fs::read(case.root.join("events/mission-malformed.jsonl"))?.is_empty());
    Ok(())
}

#[test]
fn canonical_backend_normatively_rejects_leading_space_on_new_event() -> TestResult {
    let case = fixture_case("pretty-new-event")?;
    let mission = MissionId::new("mission-pretty-new-event")?;
    let canonical = event(
        "evt_0000000000000011",
        "mission.started",
        1,
        mission.as_str(),
    );
    let padded = [b" ".as_slice(), canonical.as_slice()].concat();
    let mut log = case.authority.open_canonical_event_log(mission)?;
    assert!(matches!(
        log.append_current_json(&padded),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    assert!(std::fs::read(case.root.join("events/mission-pretty-new-event.jsonl"))?.is_empty());
    Ok(())
}

#[test]
fn canonical_backend_accepts_largest_go_scanner_readable_json_record() -> TestResult {
    let case = fixture_case("scanner-limit")?;
    let mission = MissionId::new("mission-scanner-limit")?;
    let source = sized_event(
        "evt_0000000000000022",
        1,
        mission.as_str(),
        GO_EVENT_JSON_CONTENT_MAX_BYTES,
    );
    assert_eq!(source.len(), GO_EVENT_JSON_CONTENT_MAX_BYTES);
    assert_eq!(
        encode_current_event(&decode_event_line(&source)?.record)?,
        source
    );
    let mut log = case.authority.open_canonical_event_log(mission)?;

    log.append_current_json(&source)?;

    assert_eq!(
        std::fs::metadata(case.root.join("events/mission-scanner-limit.jsonl"))?.len(),
        1024 * 1024
    );
    Ok(())
}

#[test]
fn canonical_backend_rejects_json_record_one_byte_beyond_go_scanner_limit() -> TestResult {
    let case = fixture_case("scanner-over-limit")?;
    let mission = MissionId::new("mission-scanner-over-limit")?;
    let source = sized_event(
        "evt_0000000000000023",
        1,
        mission.as_str(),
        GO_EVENT_JSON_CONTENT_MAX_BYTES + 1,
    );
    assert_eq!(source.len(), 1024 * 1024);
    assert_eq!(
        encode_current_event(&decode_event_line(&source)?.record)?,
        source
    );
    let mut log = case.authority.open_canonical_event_log(mission)?;

    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    assert!(std::fs::read(case.root.join("events/mission-scanner-over-limit.jsonl"))?.is_empty());
    Ok(())
}

#[test]
fn canonical_backend_accepts_exact_random_and_signed_decimal_new_event_ids() -> TestResult {
    let case = fixture_case("valid-new-event-ids")?;
    let mission = MissionId::new("mission-valid-new-event-ids")?;
    let mut log = case.authority.open_canonical_event_log(mission.clone())?;
    for (id, sequence) in [
        ("evt_abcdef0123456789", 1),
        ("evt_-1", 2),
        ("evt_9223372036854775807", 3),
        ("evt_-9223372036854775808", 4),
    ] {
        let encoded = event(id, "mission.started", sequence, mission.as_str());
        log.append_current_json(&encoded)?;
    }
    assert_eq!(log.next_sequence(), Some(5));
    Ok(())
}

#[test]
fn canonical_backend_rejects_noncanonical_or_out_of_range_new_event_ids() -> TestResult {
    let case = fixture_case("invalid-new-event-ids")?;
    let mission = MissionId::new("mission-invalid-new-event-ids")?;
    let mut log = case.authority.open_canonical_event_log(mission.clone())?;
    for id in [
        "evt_01",
        "evt_+1",
        "evt_-0",
        "evt_9223372036854775808",
        "evt_-9223372036854775809",
        "evt_ABCDEF0123456789",
        "evt_abcdef01234567890",
    ] {
        let encoded = event(id, "mission.started", 1, mission.as_str());
        assert!(matches!(
            log.append_current_json(&encoded),
            Err(CanonicalEventLogError::InvalidEventId)
        ));
    }
    assert!(
        std::fs::read(case.root.join("events/mission-invalid-new-event-ids.jsonl"))?.is_empty()
    );
    Ok(())
}

#[test]
fn canonical_backend_rejects_cross_mission_identity_without_writing() -> TestResult {
    let case = fixture_case("identity")?;
    let mission = MissionId::new("mission-identity")?;
    let mut log = case.authority.open_canonical_event_log(mission)?;
    let wrong = event(
        "evt_0000000000000005",
        "mission.started",
        1,
        "different-mission",
    );
    assert!(matches!(
        log.append_current_json(&wrong),
        Err(CanonicalEventLogError::MissionMismatch)
    ));
    assert!(std::fs::read(case.root.join("events/mission-identity.jsonl"))?.is_empty());
    Ok(())
}

#[test]
fn canonical_backend_rejects_ambiguous_duplicate_sequence_on_reopen() -> TestResult {
    let case = fixture_case("ambiguous")?;
    let mission = MissionId::new("mission-ambiguous")?;
    let first = event(
        "evt_0000000000000006",
        "mission.started",
        1,
        mission.as_str(),
    );
    {
        let mut log = case.authority.open_canonical_event_log(mission.clone())?;
        log.append_current_json(&first)?;
    }
    let duplicate_sequence = event("evt_0000000000000007", "phase.started", 1, mission.as_str());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(case.root.join("events/mission-ambiguous.jsonl"))?;
    file.write_all(&duplicate_sequence)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);

    assert!(matches!(
        case.authority.open_canonical_event_log(mission),
        Err(CanonicalEventLogError::DuplicateSequence)
    ));
    Ok(())
}

#[test]
fn canonical_backend_rejects_ambiguous_duplicate_event_id_on_reopen() -> TestResult {
    let case = fixture_case("duplicate-id")?;
    let mission = MissionId::new("mission-duplicate-id")?;
    let first = event("evt_same", "mission.started", 1, mission.as_str());
    let second = event("evt_same", "phase.started", 2, mission.as_str());
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let path = case.root.join("events/mission-duplicate-id.jsonl");
    let source = [first.as_slice(), b"\n", second.as_slice(), b"\n"].concat();
    std::fs::write(&path, &source)?;

    assert!(matches!(
        case.authority.open_canonical_event_log(mission),
        Err(CanonicalEventLogError::DuplicateEventId)
    ));
    assert_eq!(std::fs::read(path)?, source);
    Ok(())
}

#[test]
fn canonical_backend_rejects_duplicate_json_keys_without_rewriting_history() -> TestResult {
    let case = fixture_case("duplicate-json-key")?;
    let mission = MissionId::new("mission-duplicate-json-key")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let source = b"{\"id\":\"evt_first\",\"type\":\"worker.completed\",\"timestamp\":\"2026-07-15T00:00:00Z\",\"sequence\":1,\"mission_id\":\"mission-duplicate-json-key\",\"data\":{\"output_len\":1,\"output_len\":2}}\n";
    let path = case.root.join("events/mission-duplicate-json-key.jsonl");
    std::fs::write(&path, source)?;

    assert!(matches!(
        case.authority.open_canonical_event_log(mission),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    assert_eq!(std::fs::read(path)?, source);
    Ok(())
}

#[test]
fn canonical_backend_rejects_duplicate_json_keys_on_new_append() -> TestResult {
    let case = fixture_case("duplicate-json-key-append")?;
    let mission = MissionId::new("mission-duplicate-json-key-append")?;
    let ambiguous = br#"{"id":"evt_0000000000000012","type":"worker.completed","timestamp":"2026-07-15T00:00:00Z","sequence":1,"mission_id":"mission-duplicate-json-key-append","data":{"output_len":1,"output_len":2}}"#;
    let mut log = case.authority.open_canonical_event_log(mission)?;

    assert!(matches!(
        log.append_current_json(ambiguous),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    assert!(
        std::fs::read(
            case.root
                .join("events/mission-duplicate-json-key-append.jsonl")
        )?
        .is_empty()
    );
    Ok(())
}

#[test]
fn canonical_backend_accepts_go_compatible_worker_envelope_shape() -> TestResult {
    let case = fixture_case("go-envelope")?;
    let mission = MissionId::new("mission-go-envelope")?;
    let source = br#"{"id":"evt_0000000000000008","type":"worker.failed","timestamp":"2026-07-15T00:00:00.123456789+12:00","sequence":1,"mission_id":"mission-go-envelope","phase_id":"phase-1","worker_id":"worker-1","data":{"error":"sanitized failure","duration":"2s","output_len":4,"exit_code":1,"stderr_tail":"bounded","attempt":2}}"#;
    let canonical = encode_current_event(&decode_event_line(source)?.record)?;
    let mut log = case.authority.open_canonical_event_log(mission)?;
    log.append_current_json(&canonical)?;

    let worker = WorkerEventEnvelope::from_json(std::str::from_utf8(&canonical)?)?;
    assert_eq!(worker.kind(), WorkerEventKind::Failed);
    assert_eq!(worker.attempt(), Some(2));
    assert_eq!(worker.preserved_source_json(), Some(canonical.as_slice()));
    assert_eq!(
        std::fs::read(case.root.join("events/mission-go-envelope.jsonl"))?,
        [canonical.as_slice(), b"\n"].concat()
    );
    Ok(())
}

#[test]
fn canonical_backend_prewrite_fault_is_known_uncommitted_and_handle_remains_usable() -> TestResult {
    let case = fixture_case("prewrite-fault")?;
    let mission = MissionId::new("mission-prewrite-fault")?;
    let source = event(
        "evt_0000000000000017",
        "mission.started",
        1,
        mission.as_str(),
    );
    let path = case.root.join("events/mission-prewrite-fault.jsonl");
    let mut log = case.authority.open_canonical_event_log(mission)?;
    log.inject_fixture_fault_once(CanonicalEventLogFault::BeforeWrite);

    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::Io { .. })
    ));
    assert!(std::fs::read(&path)?.is_empty());
    log.append_current_json(&source)?;
    assert_eq!(std::fs::read(path)?, [source.as_slice(), b"\n"].concat());
    Ok(())
}

#[test]
fn canonical_backend_partial_write_fault_poisoning_blocks_append_and_reopen() -> TestResult {
    let case = fixture_case("partial-write-fault")?;
    let mission = MissionId::new("mission-partial-write-fault")?;
    let source = event(
        "evt_0000000000000018",
        "mission.started",
        1,
        mission.as_str(),
    );
    let path = case.root.join("events/mission-partial-write-fault.jsonl");
    let mut log = case.authority.open_canonical_event_log(mission.clone())?;
    log.inject_fixture_fault_once(CanonicalEventLogFault::AfterPartialWrite { bytes: 7 });

    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::AppendIndeterminate { .. })
    ));
    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::RecoveryRequired)
    ));
    assert_eq!(std::fs::read(&path)?, source[..7].to_vec());
    drop(log);
    assert!(matches!(
        case.authority.open_canonical_event_log(mission),
        Err(CanonicalEventLogError::InvalidEvent)
    ));
    Ok(())
}

#[test]
fn canonical_backend_separator_only_partial_write_recovers_complete_eof_record() -> TestResult {
    let case = fixture_case("separator-only-fault")?;
    let mission = MissionId::new("mission-separator-only-fault")?;
    drop(case.authority.open_canonical_event_log(mission.clone())?);
    let first = event(
        "evt_0000000000000024",
        "mission.started",
        1,
        mission.as_str(),
    );
    let second = event("evt_0000000000000025", "phase.started", 2, mission.as_str());
    let path = case.root.join("events/mission-separator-only-fault.jsonl");
    std::fs::write(&path, &first)?;
    let mut log = case.authority.open_canonical_event_log(mission.clone())?;
    log.inject_fixture_fault_once(CanonicalEventLogFault::AfterPartialWrite { bytes: 1 });

    assert!(matches!(
        log.append_current_json(&second),
        Err(CanonicalEventLogError::AppendIndeterminate { .. })
    ));
    assert_eq!(std::fs::read(&path)?, [first.as_slice(), b"\n"].concat());
    drop(log);

    let mut recovered = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(recovered.events().len(), 1);
    assert_eq!(recovered.next_sequence(), Some(2));
    recovered.append_current_json(&second)?;
    assert_eq!(
        std::fs::read(path)?,
        [first.as_slice(), b"\n", second.as_slice(), b"\n"].concat()
    );
    Ok(())
}

#[test]
fn canonical_backend_post_sync_ack_loss_requires_reopen_and_recovers_commit() -> TestResult {
    let case = fixture_case("post-sync-ack-loss")?;
    let mission = MissionId::new("mission-post-sync-ack-loss")?;
    let source = event(
        "evt_0000000000000019",
        "mission.started",
        1,
        mission.as_str(),
    );
    let mut log = case.authority.open_canonical_event_log(mission.clone())?;
    log.inject_fixture_fault_once(CanonicalEventLogFault::AfterSyncBeforeAcknowledge);

    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::AppendIndeterminate { .. })
    ));
    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::RecoveryRequired)
    ));
    drop(log);
    let reopened = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(reopened.events().len(), 1);
    assert_eq!(reopened.next_sequence(), Some(2));
    Ok(())
}

#[test]
fn canonical_backend_post_sync_path_replacement_requires_reopen_and_recovers() -> TestResult {
    let case = fixture_case("post-sync-path-replacement")?;
    let mission = MissionId::new("mission-post-sync-path-replacement")?;
    let source = event(
        "evt_0000000000000020",
        "mission.started",
        1,
        mission.as_str(),
    );
    let seam_residue = case
        .root
        .join("events/.mission-post-sync-path-replacement.jsonl.fixture-replacement");
    let mut log = case.authority.open_canonical_event_log(mission.clone())?;
    write_private(&seam_residue, b"stale fixture seam residue")?;
    log.inject_fixture_fault_once(CanonicalEventLogFault::ReplacePathAfterSync);

    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::AppendIdentityIndeterminate)
    ));
    assert!(matches!(
        log.append_current_json(&source),
        Err(CanonicalEventLogError::RecoveryRequired)
    ));
    drop(log);
    let reopened = case.authority.open_canonical_event_log(mission)?;
    assert_eq!(reopened.events().len(), 1);
    assert_eq!(reopened.next_sequence(), Some(2));
    assert!(!seam_residue.exists());
    Ok(())
}
