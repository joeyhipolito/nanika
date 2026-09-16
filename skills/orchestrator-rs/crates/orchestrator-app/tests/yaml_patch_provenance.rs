mod support;

use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use support::oracle::unique_root;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchManifest {
    schema_version: u64,
    #[serde(rename = "crate")]
    crate_name: String,
    version: String,
    upstream_archive: String,
    upstream_sha256: String,
    source_root: String,
    original_files: Vec<OriginalFile>,
    changes: Vec<ChangedFile>,
    unchanged_count: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalFile {
    path: String,
    mode: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangedFile {
    path: String,
    before_sha256: String,
    after_sha256: String,
}

#[test]
fn committed_archive_and_patch_delta_match_manifest() -> TestResult {
    let dependency = dependency_root();
    let manifest = load_manifest(&dependency)?;
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.crate_name, "serde-saphyr");
    assert_eq!(manifest.version, "0.0.29");
    assert_eq!(manifest.original_files.len(), 557);
    assert_eq!(manifest.changes.len(), 13);
    assert_eq!(manifest.unchanged_count, 544);

    let archive = dependency.join(&manifest.upstream_archive);
    assert_eq!(
        sha256_files(std::slice::from_ref(&archive))?[0],
        manifest.upstream_sha256
    );
    let root = unique_root("yaml-provenance")?;
    let output = Command::new("/usr/bin/tar")
        .args(["-xpf"])
        .arg(&archive)
        .arg("-C")
        .arg(&root)
        .env_clear()
        .output()?;
    require_success("extracting committed upstream crate", &output)?;
    let extracted = root.join("serde-saphyr-0.0.29");
    verify_original(&extracted, &manifest)?;
    verify_patch(&dependency.join(&manifest.source_root), &manifest)?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn patch_verifier_rejects_changed_unchanged_extra_and_missing_files() -> TestResult {
    let dependency = dependency_root();
    let manifest = load_manifest(&dependency)?;
    let root = unique_root("yaml-patch-tamper")?;
    let source = root.join("source");
    copy_tree(&dependency.join(&manifest.source_root), &source)?;
    verify_patch(&source, &manifest)?;

    let changed = &manifest.changes[0].path;
    let changed_bytes = fs::read(source.join(changed))?;
    fs::write(source.join(changed), b"tampered changed source")?;
    assert!(verify_patch(&source, &manifest).is_err());
    fs::write(source.join(changed), changed_bytes)?;

    let changed_names = manifest
        .changes
        .iter()
        .map(|change| change.path.as_str())
        .collect::<BTreeSet<_>>();
    let unchanged = manifest
        .original_files
        .iter()
        .find(|file| !changed_names.contains(file.path.as_str()))
        .ok_or("manifest has no unchanged file")?;
    let unchanged_bytes = fs::read(source.join(&unchanged.path))?;
    fs::write(source.join(&unchanged.path), b"tampered unchanged source")?;
    assert!(verify_patch(&source, &manifest).is_err());
    fs::write(source.join(&unchanged.path), unchanged_bytes)?;

    fs::write(source.join("unexpected.txt"), b"extra")?;
    assert!(verify_patch(&source, &manifest).is_err());
    fs::remove_file(source.join("unexpected.txt"))?;

    fs::remove_file(source.join(&unchanged.path))?;
    assert!(verify_patch(&source, &manifest).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn patched_source_mode_policy_matches_git_and_rejects_unsafe_bits() -> TestResult {
    for (actual, expected) in [
        (0o400, "0644"),
        (0o444, "0644"),
        (0o600, "0644"),
        (0o644, "0644"),
        (0o500, "0755"),
        (0o555, "0755"),
        (0o700, "0755"),
        (0o755, "0755"),
    ] {
        verify_patched_source_mode("fixture", actual, expected)?;
    }

    for (actual, expected) in [
        (0o544, "0644"),
        (0o455, "0755"),
        (0o411, "0644"),
        (0o664, "0644"),
        (0o646, "0644"),
        (0o4644, "0644"),
        (0o2644, "0644"),
        (0o1644, "0644"),
    ] {
        assert!(verify_patched_source_mode("fixture", actual, expected).is_err());
    }
    Ok(())
}

fn dependency_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third-party/serde-saphyr-0.0.29-msrv185")
}

fn load_manifest(root: &Path) -> TestResult<PatchManifest> {
    Ok(serde_json::from_slice(&fs::read(
        root.join("patch-manifest.json"),
    )?)?)
}

fn verify_original(root: &Path, manifest: &PatchManifest) -> TestResult {
    let actual = tree_files(root)?;
    let expected = manifest
        .original_files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    if actual.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return Err("upstream archive file set differs from patch manifest".into());
    }
    let paths = actual
        .iter()
        .map(|path| root.join(path))
        .collect::<Vec<_>>();
    let hashes = sha256_files(&paths)?;
    let by_path = manifest
        .original_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    for (path, hash) in actual.iter().zip(hashes) {
        let expected = by_path[path.as_str()];
        let mode = fs::metadata(root.join(path))?.permissions().mode() & 0o777;
        if format!("{mode:04o}") != expected.mode || hash != expected.sha256 {
            return Err(format!("upstream archive mismatch for {path}").into());
        }
    }
    Ok(())
}

fn verify_patch(root: &Path, manifest: &PatchManifest) -> TestResult {
    let actual = tree_files(root)?;
    let originals = manifest
        .original_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    if actual.iter().map(String::as_str).collect::<BTreeSet<_>>()
        != originals.keys().copied().collect::<BTreeSet<_>>()
    {
        return Err("patched source file set differs from upstream".into());
    }
    let changes = manifest
        .changes
        .iter()
        .map(|change| (change.path.as_str(), change))
        .collect::<BTreeMap<_, _>>();
    if changes.len() != manifest.changes.len()
        || manifest.unchanged_count + changes.len() != originals.len()
    {
        return Err("patch manifest counts or change identities are inconsistent".into());
    }
    for (path, change) in &changes {
        if originals
            .get(path)
            .is_none_or(|original| original.sha256 != change.before_sha256)
        {
            return Err(format!("patch before hash mismatch for {path}").into());
        }
    }
    let paths = actual
        .iter()
        .map(|path| root.join(path))
        .collect::<Vec<_>>();
    let hashes = sha256_files(&paths)?;
    for (path, hash) in actual.iter().zip(hashes) {
        let expected = changes
            .get(path.as_str())
            .map_or(originals[path.as_str()].sha256.as_str(), |change| {
                change.after_sha256.as_str()
            });
        if hash != expected {
            return Err(format!("patched source hash mismatch for {path}").into());
        }
        let mode = fs::metadata(root.join(path))?.permissions().mode() & 0o7777;
        verify_patched_source_mode(path, mode, &originals[path.as_str()].mode)?;
    }
    Ok(())
}

fn verify_patched_source_mode(path: &str, actual: u32, expected: &str) -> TestResult {
    let expected = u32::from_str_radix(expected, 8)
        .map_err(|_| format!("invalid upstream mode for {path}"))?;
    if !matches!(expected, 0o644 | 0o755) {
        return Err(format!("unsupported upstream mode for {path}").into());
    }
    if actual & 0o7022 != 0 {
        return Err(format!("unsafe patched source mode for {path}").into());
    }
    let expected_owner_executable = expected & 0o100 != 0;
    let actual_owner_executable = actual & 0o100 != 0;
    let stray_group_or_world_execute = !actual_owner_executable && actual & 0o011 != 0;
    if actual_owner_executable != expected_owner_executable || stray_group_or_world_execute {
        return Err(format!("patched source executable class mismatch for {path}").into());
    }
    Ok(())
}

fn tree_files(root: &Path) -> TestResult<Vec<String>> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<String>) -> TestResult {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "unexpected symlink in dependency source: {:?}",
                    entry.path()
                )
                .into());
            }
            if metadata.is_dir() {
                visit(root, &entry.path(), files)?;
            } else if metadata.is_file() {
                files.push(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            } else {
                return Err(format!("unsupported dependency file type: {:?}", entry.path()).into());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn sha256_files(paths: &[PathBuf]) -> TestResult<Vec<String>> {
    let mut command = Command::new("/usr/bin/shasum");
    command.args(["-a", "256", "--"]);
    command.args(paths);
    command.env_clear();
    let output = command.output()?;
    require_success("hashing dependency source", &output)?;
    let hashes = String::from_utf8(output.stdout)?
        .lines()
        .map(|line| {
            line.split_whitespace()
                .next()
                .ok_or("malformed shasum output")
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if hashes.len() != paths.len() {
        return Err("shasum result count mismatch".into());
    }
    Ok(hashes)
}

fn copy_tree(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
            let mut permissions = fs::metadata(&target)?.permissions();
            permissions.set_mode((permissions.mode() & 0o7777) | 0o200);
            fs::set_permissions(&target, permissions)?;
        }
    }
    Ok(())
}

fn require_success(context: &str, output: &std::process::Output) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{context} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}
