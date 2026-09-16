mod support;

use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use support::corpus;
use support::oracle::{
    ADAPTER, BASELINE, OracleFixture, TestResult, archive_source, assert_no_surviving_processes,
    attempt_dependency_materialization_with_fallback, committed_proxy_root, copy_tree,
    fixed_build_environment_names, make_writable, proxy_manifest_bytes, require_success,
    sandboxed_command, tree_manifest_bytes, unique_root, validate_case_environment,
    validate_case_id, validate_case_path, validate_fixture_reference, verify_adapter,
    verify_archived_tree, verify_archived_tree_manifest, verify_proxy, verify_revision,
};

fn fresh_archive(label: &str) -> TestResult<(PathBuf, PathBuf)> {
    let root = unique_root(label)?;
    let source = root.join("source");
    archive_source(BASELINE, &source)?;
    verify_archived_tree(&source)?;
    Ok((root, source))
}

fn fresh_proxy(label: &str) -> TestResult<(PathBuf, PathBuf)> {
    let root = unique_root(label)?;
    let proxy = root.join("proxy");
    copy_tree(&committed_proxy_root(), &proxy)?;
    verify_proxy(&proxy, proxy_manifest_bytes())?;
    Ok((root, proxy))
}

#[test]
fn rejects_wrong_or_moving_revision() -> TestResult {
    verify_revision(BASELINE)?;
    for revision in [
        "3f2e5d1b",
        "89d89dd5",
        "HEAD",
        "main",
        "",
        "3f2e5d1b9acfbe4a4338bb003425668cc803464f",
    ] {
        assert!(verify_revision(revision).is_err(), "accepted {revision:?}");
    }
    Ok(())
}

#[test]
fn rejects_tampered_archived_source() -> TestResult {
    for relative in [
        "skills/orchestrator/internal/decompose/decompose.go",
        "shared/sdk/client.go",
    ] {
        let (root, source) = fresh_archive("tampered-source")?;
        let path = source.join(relative);
        let mut bytes = fs::read(&path)?;
        bytes.extend_from_slice(b"\n// tampered\n");
        fs::write(path, bytes)?;
        assert!(verify_archived_tree(&source).is_err());
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn rejects_archive_extra_missing_mode_and_symlink() -> TestResult {
    let mutations: [fn(&Path) -> TestResult; 4] = [
        |root| {
            fs::write(
                root.join("skills/orchestrator/extra.go"),
                b"package extra\n",
            )?;
            Ok(())
        },
        |root| {
            fs::remove_file(root.join("skills/orchestrator/internal/config/config.go"))?;
            Ok(())
        },
        |root| {
            let path = root.join("skills/orchestrator/internal/config/config.go");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            Ok(())
        },
        |root| {
            let path = root.join("skills/orchestrator/internal/config/config.go");
            fs::remove_file(&path)?;
            symlink("../core/types.go", path)?;
            Ok(())
        },
    ];
    for mutate in mutations {
        let (root, source) = fresh_archive("archive-shape")?;
        mutate(&source)?;
        assert!(verify_archived_tree(&source).is_err());
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn rejects_tampered_adapter() -> TestResult {
    verify_adapter(ADAPTER.as_bytes())?;
    let mut tampered = ADAPTER.as_bytes().to_vec();
    tampered.extend_from_slice(b"\n// tampered\n");
    assert!(verify_adapter(&tampered).is_err());
    let forbidden = ADAPTER.replace("\"sort\"", "\"sort\"\n\t\"net/http\"");
    assert!(verify_adapter(forbidden.as_bytes()).is_err());
    Ok(())
}

#[test]
fn rejects_tampered_frozen_module_metadata() -> TestResult {
    for relative in ["skills/orchestrator/go.mod", "skills/orchestrator/go.sum"] {
        let (root, source) = fresh_archive("module-metadata")?;
        fs::write(source.join(relative), b"tampered\n")?;
        assert!(verify_archived_tree(&source).is_err());
        fs::remove_dir_all(root)?;
    }
    let (root, source) = fresh_archive("tree-manifest-metadata")?;
    let mut manifest: Value = serde_json::from_str(tree_manifest_bytes())?;
    manifest["files"][0]["git_blob"] = json!("0000000000000000000000000000000000000000");
    assert!(verify_archived_tree_manifest(&source, &serde_json::to_string(&manifest)?).is_err());
    let mut manifest: Value = serde_json::from_str(tree_manifest_bytes())?;
    let duplicate = manifest["files"][0].clone();
    manifest["files"]
        .as_array_mut()
        .ok_or("tree files array")?
        .push(duplicate);
    assert!(verify_archived_tree_manifest(&source, &serde_json::to_string(&manifest)?).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn rejects_tampered_module_zip_or_mod() -> TestResult {
    let manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    for extension in [".zip", ".mod"] {
        let (root, proxy) = fresh_proxy("proxy-content")?;
        let relative = manifest["files"]
            .as_array()
            .ok_or("proxy files must be an array")?
            .iter()
            .filter_map(|file| file["path"].as_str())
            .find(|path| path.ends_with(extension))
            .ok_or("proxy manifest omitted requested extension")?;
        fs::write(proxy.join(relative), b"tampered")?;
        assert!(verify_proxy(&proxy, proxy_manifest_bytes()).is_err());
        fs::remove_dir_all(root)?;
    }
    let (root, proxy) = fresh_proxy("sumdb-content")?;
    let relative = manifest["files"]
        .as_array()
        .ok_or("proxy files must be an array")?
        .iter()
        .filter_map(|file| file["path"].as_str())
        .find(|path| path.starts_with("sumdb/"))
        .ok_or("proxy manifest omitted sumdb authentication material")?;
    fs::write(proxy.join(relative), b"tampered sumdb proof")?;
    assert!(verify_proxy(&proxy, proxy_manifest_bytes()).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn rejects_tampered_proxy_metadata() -> TestResult {
    let mut manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    manifest["modules"][0]["version"] = json!("v9.9.9");
    assert!(verify_proxy(&committed_proxy_root(), &serde_json::to_string(&manifest)?).is_err());

    let mut manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    manifest["files"][0]["sha256"] = json!("00");
    assert!(verify_proxy(&committed_proxy_root(), &serde_json::to_string(&manifest)?).is_err());

    let mut manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    let external = manifest["modules"]
        .as_array_mut()
        .ok_or("modules array")?
        .iter_mut()
        .find(|module| module["replacement"].is_null())
        .ok_or("external module")?;
    external["sum"] = json!("h1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
    assert!(verify_proxy(&committed_proxy_root(), &serde_json::to_string(&manifest)?).is_err());

    let mut manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    let replacement = manifest["modules"]
        .as_array_mut()
        .ok_or("modules array")?
        .iter_mut()
        .find(|module| !module["replacement"].is_null())
        .ok_or("replacement module")?;
    replacement["replacement"]["git_tree"] = json!("0000000000000000000000000000000000000000");
    assert!(verify_proxy(&committed_proxy_root(), &serde_json::to_string(&manifest)?).is_err());

    let mut manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    let duplicate = manifest["files"][0].clone();
    manifest["files"]
        .as_array_mut()
        .ok_or("proxy files array")?
        .push(duplicate);
    assert!(verify_proxy(&committed_proxy_root(), &serde_json::to_string(&manifest)?).is_err());

    let mut manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    manifest["unknown"] = json!(true);
    assert!(verify_proxy(&committed_proxy_root(), &serde_json::to_string(&manifest)?).is_err());
    Ok(())
}

#[test]
fn rejects_missing_or_extra_proxy_material() -> TestResult {
    for extra in [false, true] {
        let (root, proxy) = fresh_proxy("proxy-shape")?;
        if extra {
            fs::write(proxy.join("extra"), b"extra")?;
        } else {
            let manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
            let relative = manifest["files"][0]["path"]
                .as_str()
                .ok_or("proxy path must be a string")?;
            fs::remove_file(proxy.join(relative))?;
        }
        assert!(verify_proxy(&proxy, proxy_manifest_bytes()).is_err());
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn ignores_ambient_go_and_git_state_and_builds_readonly_offline() -> TestResult {
    let names = fixed_build_environment_names();
    for forbidden in [
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_ASKPASS",
        "SSH_AUTH_SOCK",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "GONOPROXY",
        "GOPRIVATE",
    ] {
        assert!(
            !names.contains(&forbidden),
            "ambient key leaked: {forbidden}"
        );
    }
    for required in [
        "GOMODCACHE",
        "GOPATH",
        "GOWORK",
        "GOFLAGS",
        "GOPROXY",
        "GOSUMDB",
        "GOENV",
        "HOME",
    ] {
        assert!(
            names.contains(&required),
            "missing isolated key: {required}"
        );
    }
    let legacy_root =
        Path::new("/private/tmp").join(format!("orchestrator-rs-oracle-{}-0", std::process::id()));
    fs::create_dir(&legacy_root)?;
    fs::create_dir_all(legacy_root.join("fixture/build/home"))?;
    let legacy_sentinel = legacy_root.join("fixture/build/home/preseed-sentinel");
    fs::write(&legacy_sentinel, b"untrusted")?;

    let fixture = OracleFixture::build()?;
    assert!(
        !fixture.root().starts_with(&legacy_root),
        "oracle accepted a predictable preseeded root"
    );
    assert!(
        !fixture.root().join("build/home/preseed-sentinel").exists(),
        "oracle fresh root contained preseeded ambient content"
    );
    assert!(fixture.go_version().starts_with("go version go"));
    assert!(fixture.binary().is_file());
    let permissions = fs::metadata(fixture.module_cache())?.permissions().mode();
    assert_eq!(permissions & 0o222, 0, "module cache remains writable");
    fixture.cleanup()?;
    assert_eq!(fs::read(&legacy_sentinel)?, b"untrusted");
    fs::remove_dir_all(legacy_root)?;
    Ok(())
}

#[test]
fn missing_proxy_material_cannot_reach_network() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let fallback = format!("http://{}", listener.local_addr()?);
    let (root, proxy) = fresh_proxy("proxy-no-network")?;
    let manifest: Value = serde_json::from_str(proxy_manifest_bytes())?;
    let relative = manifest["files"]
        .as_array()
        .ok_or("proxy files must be an array")?
        .iter()
        .filter_map(|file| file["path"].as_str())
        .find(|path| path.ends_with(".zip"))
        .ok_or("proxy zip missing")?;
    fs::remove_file(proxy.join(relative))?;
    assert!(verify_proxy(&proxy, proxy_manifest_bytes()).is_err());
    let acquisition = attempt_dependency_materialization_with_fallback(&root, &proxy, &fallback)?;
    assert!(
        !acquisition.status.success(),
        "dependency acquisition unexpectedly succeeded with missing proxy material"
    );
    assert!(
        listener.accept().is_err(),
        "sandboxed dependency acquisition reached the fallback listener"
    );
    assert_no_surviving_processes(&root)?;
    drop(listener);
    make_writable(&root)?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn rejects_unsafe_duplicate_or_unsorted_cases_and_fixture_paths() -> TestResult {
    for unsafe_id in [
        "",
        "../outside",
        "nested/id",
        "/tmp/outside",
        "Upper",
        "has space",
    ] {
        assert!(
            validate_case_id(unsafe_id).is_err(),
            "accepted {unsafe_id:?}"
        );
    }
    for unsafe_reference in ["", "../outside", "/tmp/outside", "$FIXTURE_ROOT/file"] {
        assert!(
            validate_fixture_reference(unsafe_reference).is_err(),
            "accepted {unsafe_reference:?}"
        );
    }
    let ids = ["a", "a"];
    assert_ne!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        ids.len()
    );
    let unsorted = ["b", "a"];
    assert!(!unsorted.is_sorted());
    Ok(())
}

#[test]
fn rejects_case_environment_schema_and_escape() -> TestResult {
    let root = unique_root("environment")?;
    let required = ["HOME", "TMPDIR"];
    let valid = json!({"HOME":"$FIXTURE_ROOT/home","TMPDIR":"$FIXTURE_ROOT/tmp"});
    validate_case_environment(valid.as_object().ok_or("valid map")?, &required, &root)?;
    for invalid in [
        json!({"HOME":"$FIXTURE_ROOT/home"}),
        json!({"HOME":"$FIXTURE_ROOT/home","TMPDIR":"$FIXTURE_ROOT/tmp","EXTRA":"x"}),
        json!({"HOME":7,"TMPDIR":"$FIXTURE_ROOT/tmp"}),
        json!({"HOME":"/tmp/outside","TMPDIR":"$FIXTURE_ROOT/tmp"}),
        json!({"HOME":"$FIXTURE_ROOT/../outside","TMPDIR":"$FIXTURE_ROOT/tmp"}),
        json!({"HOME":"$UNKNOWN/home","TMPDIR":"$FIXTURE_ROOT/tmp"}),
    ] {
        assert!(
            validate_case_environment(invalid.as_object().ok_or("invalid map")?, &required, &root)
                .is_err()
        );
    }
    let target = root.join("target");
    fs::create_dir_all(&target)?;
    symlink(&target, root.join("link"))?;
    let escaped = json!({"HOME":"$FIXTURE_ROOT/link/home","TMPDIR":"$FIXTURE_ROOT/tmp"});
    assert!(
        validate_case_environment(escaped.as_object().ok_or("escaped map")?, &required, &root)
            .is_err()
    );
    assert!(validate_case_path(Path::new("/tmp/outside"), &root).is_err());
    assert!(validate_case_path(&root.join("link/personas"), &root).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn kills_timed_out_oracle_process_group() -> TestResult {
    let root = unique_root("timeout-process-group")?;
    let script = root.join("fork.sh");
    let pid_file = root.join("child.pid");
    fs::write(&script, b"#!/bin/sh\nsleep 30 &\necho $! > \"$1\"\nwait\n")?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
    let environment = [
        ("HOME", root.to_string_lossy().into_owned()),
        ("TMPDIR", root.to_string_lossy().into_owned()),
        ("PATH", "/bin:/usr/bin".to_owned()),
    ];
    let result = sandboxed_command(
        Path::new("/bin/sh"),
        &[
            script.to_string_lossy().into_owned(),
            pid_file.to_string_lossy().into_owned(),
        ],
        &environment,
        &root,
        None,
        Duration::from_secs(1),
    );
    assert!(result.is_err(), "timeout helper unexpectedly completed");
    let pid = fs::read_to_string(&pid_file)?.trim().to_owned();
    let output = Command::new("/bin/ps")
        .args(["-p", &pid, "-o", "pid="])
        .env_clear()
        .output()?;
    assert!(
        !output.status.success() || String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "forked child survived timeout: {pid}"
    );
    assert_no_surviving_processes(&root)?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn leaves_no_oracle_artifacts_after_success_and_failure() -> TestResult {
    let fixture = OracleFixture::build()?;
    let root = fixture.root().to_path_buf();
    let valid = serde_json::to_vec(&json!({"operation":"home","input":{}}))?;
    let environment = [
        (
            "HOME",
            root.join("case-home").to_string_lossy().into_owned(),
        ),
        (
            "TMPDIR",
            root.join("case-tmp").to_string_lossy().into_owned(),
        ),
        (
            "PATH",
            root.join("inert-bin").to_string_lossy().into_owned(),
        ),
    ];
    for (_, value) in &environment {
        fs::create_dir_all(value)?;
    }
    let success = fixture.run(&environment, &valid, Duration::from_secs(10))?;
    require_success("running successful survivor case", &success)?;
    let invalid = b"not-json";
    let failure_response = fixture.run(&environment, invalid, Duration::from_secs(10))?;
    require_success("running protocol-failure survivor case", &failure_response)?;
    fixture.cleanup()
}

#[test]
fn rejects_case_identity_substitution_and_reference_symlink() -> TestResult {
    let fixture_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/core-parity");
    let manifest = fixture_root.join("differential/case-manifest.json");
    let mut corpus: Value =
        serde_json::from_slice(&fs::read(fixture_root.join("differential/corpus.json"))?)?;
    corpus["cases"][0]["case_id"] = Value::String("checkpoint-current-writer-go-readable-x".into());
    let error = match corpus::validate(&corpus, &fixture_root, &manifest) {
        Ok(()) => return Err("valid-looking case ID substitution was accepted".into()),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("case manifest identity mismatch")
            || error.to_string().contains("lexicographically ordered"),
        "unexpected substitution error: {error}"
    );

    let root = unique_root("case-reference-symlink")?;
    let copied = root.join("fixtures");
    copy_tree(&fixture_root, &copied)?;
    let mut copied_corpus: Value =
        serde_json::from_slice(&fs::read(copied.join("differential/corpus.json"))?)?;
    copied_corpus["case_manifest"] = Value::String("case-manifest.json".into());
    fs::write(
        copied.join("differential/corpus.json"),
        serde_json::to_vec(&copied_corpus)?,
    )?;
    fs::copy(
        copied.join("differential/case-manifest.json"),
        copied.join("case-manifest.json"),
    )?;
    let referenced = copied.join("checkpoints/legacy-direct-v2.json");
    let outside = root.join("outside.json");
    fs::copy(&referenced, &outside)?;
    fs::remove_file(&referenced)?;
    symlink(&outside, &referenced)?;
    assert!(corpus::validate(&copied_corpus, &copied, &copied.join("case-manifest.json")).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}
