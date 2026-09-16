#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use orchestrator_exec::{ProcessPurpose, ProcessRequest, ServiceContractError};

fn request(
    purpose: ProcessPurpose,
    root: OsString,
) -> Result<ProcessRequest, ServiceContractError> {
    ProcessRequest::new(purpose, "enrolled-provider", root)?
        .with_argument("--model")?
        .with_argument("secret-model")?
        .with_environment("TOKEN", "secret-token")?
        .with_environment("MODE", "batch")?
        .with_stdin(b"secret-stdin".to_vec())?
        .with_max_output_bytes(65_537)
}

fn standard_request() -> Result<ProcessRequest, ServiceContractError> {
    request(
        ProcessPurpose::ProviderWorker,
        OsString::from("/secret/root"),
    )
}

#[test]
fn identical_semantics_have_identical_opaque_fingerprints() -> Result<(), ServiceContractError> {
    let first = standard_request()?.fingerprint();
    let second = standard_request()?.fingerprint();

    assert_eq!(first, second);
    let rendered = format!("{first:?}");
    assert_eq!(rendered, "ProcessRequestFingerprint([REDACTED])");
    assert!(!rendered.contains("secret"));
    Ok(())
}

#[test]
fn every_scalar_semantic_changes_the_fingerprint() -> Result<(), ServiceContractError> {
    let baseline = standard_request()?.fingerprint();
    let cases = [
        request(ProcessPurpose::Git, OsString::from("/secret/root"))?.fingerprint(),
        ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "different-provider",
            "/secret/root",
        )?
        .with_argument("--model")?
        .with_argument("secret-model")?
        .with_environment("TOKEN", "secret-token")?
        .with_environment("MODE", "batch")?
        .with_stdin(b"secret-stdin".to_vec())?
        .with_max_output_bytes(65_537)?
        .fingerprint(),
        request(
            ProcessPurpose::ProviderWorker,
            OsString::from("/secret/other-root"),
        )?
        .fingerprint(),
        standard_request()?
            .with_max_output_bytes(65_538)?
            .fingerprint(),
        standard_request()?
            .with_truncated_output_acknowledged()
            .fingerprint(),
    ];

    for changed in cases {
        assert_ne!(baseline, changed);
    }
    Ok(())
}

#[test]
fn ordered_arguments_and_environment_pairs_are_bound() -> Result<(), ServiceContractError> {
    let baseline = standard_request()?.fingerprint();
    let reversed_arguments = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("secret-model")?
    .with_argument("--model")?
    .with_environment("TOKEN", "secret-token")?
    .with_environment("MODE", "batch")?
    .with_stdin(b"secret-stdin".to_vec())?
    .with_max_output_bytes(65_537)?
    .fingerprint();
    let reversed_environment = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("--model")?
    .with_argument("secret-model")?
    .with_environment("MODE", "batch")?
    .with_environment("TOKEN", "secret-token")?
    .with_stdin(b"secret-stdin".to_vec())?
    .with_max_output_bytes(65_537)?
    .fingerprint();

    assert_ne!(baseline, reversed_arguments);
    assert_ne!(baseline, reversed_environment);
    Ok(())
}

#[test]
fn substitutions_and_collection_boundaries_cannot_collide() -> Result<(), ServiceContractError> {
    let baseline = standard_request()?.fingerprint();
    let changed_argument = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("--model")?
    .with_argument("other-model")?
    .with_environment("TOKEN", "secret-token")?
    .with_environment("MODE", "batch")?
    .with_stdin(b"secret-stdin".to_vec())?
    .with_max_output_bytes(65_537)?
    .fingerprint();
    let changed_environment_key = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("--model")?
    .with_argument("secret-model")?
    .with_environment("OTHER", "secret-token")?
    .with_environment("MODE", "batch")?
    .with_stdin(b"secret-stdin".to_vec())?
    .with_max_output_bytes(65_537)?
    .fingerprint();
    let changed_environment_value = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("--model")?
    .with_argument("secret-model")?
    .with_environment("TOKEN", "other-token")?
    .with_environment("MODE", "batch")?
    .with_stdin(b"secret-stdin".to_vec())?
    .with_max_output_bytes(65_537)?
    .fingerprint();
    let changed_stdin = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("--model")?
    .with_argument("secret-model")?
    .with_environment("TOKEN", "secret-token")?
    .with_environment("MODE", "batch")?
    .with_stdin(b"other-stdin".to_vec())?
    .with_max_output_bytes(65_537)?
    .fingerprint();

    for changed in [
        changed_argument,
        changed_environment_key,
        changed_environment_value,
        changed_stdin,
    ] {
        assert_ne!(baseline, changed);
    }

    let split = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("ab")?
    .with_argument("c")?
    .fingerprint();
    let differently_split = ProcessRequest::new(
        ProcessPurpose::ProviderWorker,
        "enrolled-provider",
        "/secret/root",
    )?
    .with_argument("a")?
    .with_argument("bc")?
    .fingerprint();
    assert_ne!(split, differently_split);
    Ok(())
}

#[test]
fn absent_stdin_differs_from_present_empty_stdin() -> Result<(), ServiceContractError> {
    let absent = ProcessRequest::new(ProcessPurpose::Tool, "tool", "/root")?.fingerprint();
    let empty = ProcessRequest::new(ProcessPurpose::Tool, "tool", "/root")?
        .with_stdin(Vec::new())?
        .fingerprint();
    assert_ne!(absent, empty);
    Ok(())
}

#[test]
fn unix_path_bytes_are_not_lossily_normalized() -> Result<(), ServiceContractError> {
    let first = request(
        ProcessPurpose::ProviderWorker,
        OsString::from_vec(b"/root/\x80".to_vec()),
    )?
    .fingerprint();
    let second = request(
        ProcessPurpose::ProviderWorker,
        OsString::from_vec(b"/root/\x81".to_vec()),
    )?
    .fingerprint();
    assert_ne!(first, second);
    Ok(())
}
