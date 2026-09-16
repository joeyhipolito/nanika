//! Local, read-only Git snapshotting for the experimental coding pilot.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use orchestrator_process::{
    CancellationToken, ProcessReport, ProcessSpec, ProcessSupervisor, ProcessTermination,
};
use sha2::{Digest, Sha256};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_MAX_OUTPUT: usize = 64 * 1024 * 1024;
const MAX_BLOB_BYTES: u64 = GIT_MAX_OUTPUT as u64;
const GIT_BATCH_ITEMS: usize = 128;

#[derive(Clone, Debug)]
struct TreeEntry {
    path: PathBuf,
    oid: String,
    executable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
pub(crate) struct WorkspaceCheckpoint {
    root: PathBuf,
    identity: DirectoryIdentity,
}

#[derive(Debug)]
pub(crate) struct GeneratedOutput {
    pub path: String,
    pub kind: &'static str,
    pub bytes: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct RepositorySnapshot {
    pub source: PathBuf,
    pub head: String,
    pub baseline: PathBuf,
    pub workspace: PathBuf,
    baseline_identity: DirectoryIdentity,
    workspace_identity: DirectoryIdentity,
}

pub(crate) fn prepare(
    requested_repo: &Path,
    output_root: &Path,
    supervisor: &ProcessSupervisor,
    cancellation: &CancellationToken,
) -> Result<RepositorySnapshot, String> {
    let metadata = fs::symlink_metadata(requested_repo)
        .map_err(|error| format!("repository {}: {error}", requested_repo.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("repository must be a real directory, not a symlink".to_owned());
    }
    let source = fs::canonicalize(requested_repo)
        .map_err(|error| format!("repository {}: {error}", requested_repo.display()))?;
    let runner = GitRunner::new(supervisor, cancellation);
    let root = runner.success_text(&source, &["rev-parse", "--show-toplevel"])?;
    let reported_root = fs::canonicalize(root.trim())
        .map_err(|error| format!("Git reported an invalid repository root: {error}"))?;
    if reported_root != source {
        return Err(format!(
            "--repo must select the repository root exactly; Git reported {}",
            reported_root.display()
        ));
    }
    let inside = runner.success_text(&source, &["rev-parse", "--is-inside-work-tree"])?;
    if inside.trim() != "true" {
        return Err("repository is not a Git working tree".to_owned());
    }
    let head = runner
        .success_text(&source, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_owned();
    ensure_clean(&runner, &source)?;
    let listing = runner.success_bytes(&source, &["ls-tree", "-rz", "--full-tree", "HEAD"])?;
    let entries = parse_tree(&listing)?;
    let blob_sizes = read_blob_sizes(&runner, &source, &entries)?;

    let baseline = output_root.join("source-head");
    let workspace = output_root.join("workspace");
    private_dir(&baseline)?;
    private_dir(&workspace)?;
    write_blob_batches(&runner, &source, &baseline, &entries, &blob_sizes)?;
    copy_tree(&baseline, &workspace, &entries)?;
    let baseline_identity = directory_identity(&baseline, "snapshot baseline")?;
    let workspace_identity = directory_identity(&workspace, "snapshot workspace")?;
    verify_source_state(&runner, &source, &head)?;
    Ok(RepositorySnapshot {
        source,
        head,
        baseline,
        workspace,
        baseline_identity,
        workspace_identity,
    })
}

impl RepositorySnapshot {
    pub(crate) fn reopen(
        source: PathBuf,
        head: String,
        baseline: PathBuf,
        workspace: PathBuf,
        baseline_identity: (u64, u64),
        workspace_identity: (u64, u64),
        supervisor: &ProcessSupervisor,
    ) -> Result<Self, String> {
        let snapshot = Self {
            source,
            head,
            baseline,
            workspace,
            baseline_identity: DirectoryIdentity {
                device: baseline_identity.0,
                inode: baseline_identity.1,
            },
            workspace_identity: DirectoryIdentity {
                device: workspace_identity.0,
                inode: workspace_identity.1,
            },
        };
        snapshot.validate_directories()?;
        snapshot.verify_source(supervisor)?;
        Ok(snapshot)
    }

    pub(crate) const fn directory_identities(&self) -> ((u64, u64), (u64, u64)) {
        (
            (self.baseline_identity.device, self.baseline_identity.inode),
            (
                self.workspace_identity.device,
                self.workspace_identity.inode,
            ),
        )
    }

    pub(crate) fn state_digests(&self) -> Result<(String, String), String> {
        self.validate_directories()?;
        Ok((
            digest_regular_tree(&self.baseline)?,
            digest_regular_tree(&self.workspace)?,
        ))
    }

    #[cfg(test)]
    pub(crate) fn from_paths_for_test(
        source: PathBuf,
        baseline: PathBuf,
        workspace: PathBuf,
    ) -> Result<Self, String> {
        Ok(Self {
            source,
            head: String::new(),
            baseline_identity: directory_identity(&baseline, "snapshot baseline")?,
            workspace_identity: directory_identity(&workspace, "snapshot workspace")?,
            baseline,
            workspace,
        })
    }

    pub(crate) fn verify_source(&self, supervisor: &ProcessSupervisor) -> Result<(), String> {
        let cancellation = CancellationToken::new();
        let runner = GitRunner::new(supervisor, &cancellation);
        verify_source_state(&runner, &self.source, &self.head)
    }

    pub(crate) fn diff(&self, supervisor: &ProcessSupervisor) -> Result<Vec<u8>, String> {
        self.validate_directories()?;
        let cancellation = CancellationToken::new();
        let runner = GitRunner::new(supervisor, &cancellation);
        let root = self
            .baseline
            .parent()
            .ok_or_else(|| "snapshot paths have no output parent".to_owned())?;
        let baseline = self
            .baseline
            .file_name()
            .ok_or_else(|| "baseline path has no name".to_owned())?;
        let workspace = self
            .workspace
            .file_name()
            .ok_or_else(|| "workspace path has no name".to_owned())?;
        let report = runner.run(
            root,
            &[
                OsStr::new("diff"),
                OsStr::new("--no-index"),
                OsStr::new("--binary"),
                OsStr::new("--no-ext-diff"),
                OsStr::new("--src-prefix=a/"),
                OsStr::new("--dst-prefix=b/"),
                OsStr::new("--"),
                baseline,
                workspace,
            ],
        )?;
        self.validate_directories()?;
        validated_diff_output(report)
    }

    /// Returns complete final text for added and modified regular files, and
    /// complete original text for deleted regular files.
    /// Symlinks and special files created by the coding process are refused.
    pub(crate) fn changed_source_context(&self, maximum: usize) -> Result<Vec<u8>, String> {
        self.validate_directories()?;
        let baseline = collect_tree(&self.baseline)?.files;
        let workspace = collect_tree(&self.workspace)?.files;
        let mut context = Vec::new();
        for path in baseline.union(&workspace) {
            let before_path = self.baseline.join(path);
            let after_path = self.workspace.join(path);
            match (baseline.contains(path), workspace.contains(path)) {
                (true, true) => {
                    if files_equal_with_mode(&before_path, &after_path)? {
                        continue;
                    }
                    // A binary original can produce a UTF-8 Git binary patch. Validate
                    // it even though only the final text is sent as source context.
                    let before = read_file_for_context(&before_path, GIT_MAX_OUTPUT)?;
                    std::str::from_utf8(&before).map_err(|_| {
                        format!("original changed source file {path:?} is not UTF-8")
                    })?;
                    drop(before);
                    let after =
                        read_file_for_context(&after_path, maximum.saturating_sub(context.len()))?;
                    append_context(
                        &mut context,
                        path,
                        "complete final contents of modified file",
                        &after,
                        maximum,
                    )?;
                }
                (false, true) => {
                    let after =
                        read_file_for_context(&after_path, maximum.saturating_sub(context.len()))?;
                    append_context(
                        &mut context,
                        path,
                        "complete final contents of added file",
                        &after,
                        maximum,
                    )?;
                }
                (true, false) => {
                    let before =
                        read_file_for_context(&before_path, maximum.saturating_sub(context.len()))?;
                    append_context(
                        &mut context,
                        path,
                        "complete original contents of deleted file",
                        &before,
                        maximum,
                    )?;
                }
                (false, false) => {
                    return Err("changed source path disappeared from both snapshots".to_owned());
                }
            }
        }
        Ok(context)
    }

    pub(crate) fn checkpoint_workspace(
        &self,
        destination: &Path,
    ) -> Result<WorkspaceCheckpoint, String> {
        self.validate_directories()?;
        private_dir(destination)?;
        copy_regular_tree(&self.workspace, &self.workspace, destination)?;
        self.validate_directories()?;
        let checkpoint = WorkspaceCheckpoint {
            root: destination.to_path_buf(),
            identity: directory_identity(destination, "reviewed workspace checkpoint")?,
        };
        checkpoint.verify_preserved(self, None)?;
        Ok(checkpoint)
    }

    fn validate_directories(&self) -> Result<(), String> {
        require_directory_identity(&self.baseline, self.baseline_identity, "snapshot baseline")?;
        require_directory_identity(
            &self.workspace,
            self.workspace_identity,
            "snapshot workspace",
        )
    }
}

fn digest_regular_tree(root: &Path) -> Result<String, String> {
    let tree = collect_tree(root)?;
    let mut digest = Sha256::new();
    for directory in &tree.directories {
        digest.update(b"directory\0");
        digest.update(directory.as_os_str().as_bytes());
        digest.update(b"\0");
    }
    for path in &tree.files {
        let full = root.join(path);
        let metadata = full
            .symlink_metadata()
            .map_err(|error| format!("inspecting {}: {error}", full.display()))?;
        digest.update(b"file\0");
        digest.update(path.as_os_str().as_bytes());
        digest.update(b"\0");
        digest.update((metadata.permissions().mode() & 0o777).to_be_bytes());
        let bytes =
            fs::read(&full).map_err(|error| format!("reading {}: {error}", full.display()))?;
        digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

impl WorkspaceCheckpoint {
    pub(crate) fn verify_after_verification(
        &self,
        snapshot: &RepositorySnapshot,
        supervisor: &ProcessSupervisor,
    ) -> Result<Vec<GeneratedOutput>, String> {
        self.verify_preserved(snapshot, Some(supervisor))
    }

    fn verify_preserved(
        &self,
        snapshot: &RepositorySnapshot,
        supervisor: Option<&ProcessSupervisor>,
    ) -> Result<Vec<GeneratedOutput>, String> {
        snapshot.validate_directories()?;
        require_directory_identity(&self.root, self.identity, "reviewed workspace checkpoint")?;
        let checkpoint = collect_tree(&self.root)?;
        let workspace = collect_tree(&snapshot.workspace)?;
        for path in &checkpoint.files {
            if !workspace.files.contains(path)
                || !files_equal_with_mode(&self.root.join(path), &snapshot.workspace.join(path))?
            {
                return Err(format!(
                    "verification changed reviewed source {}",
                    path.display()
                ));
            }
        }
        let generated_files: Vec<PathBuf> = workspace
            .files
            .difference(&checkpoint.files)
            .cloned()
            .collect();
        let generated_directories: Vec<PathBuf> = workspace
            .directories
            .difference(&checkpoint.directories)
            .cloned()
            .collect();
        if let Some(supervisor) = supervisor {
            require_ignored_outputs(snapshot, supervisor, &generated_files)?;
        } else if !generated_files.is_empty() || !generated_directories.is_empty() {
            return Err("reviewed workspace checkpoint did not match its source".to_owned());
        }
        snapshot.validate_directories()?;
        require_directory_identity(&self.root, self.identity, "reviewed workspace checkpoint")?;
        let mut generated = Vec::with_capacity(generated_files.len() + generated_directories.len());
        for path in generated_directories {
            generated.push(GeneratedOutput {
                path: utf8_path(&path)?,
                kind: "directory",
                bytes: None,
            });
        }
        for path in generated_files {
            let bytes = snapshot
                .workspace
                .join(&path)
                .symlink_metadata()
                .map_err(|error| format!("inspecting {}: {error}", path.display()))?
                .len();
            generated.push(GeneratedOutput {
                path: utf8_path(&path)?,
                kind: "file",
                bytes: Some(bytes),
            });
        }
        generated.sort_by(|left, right| left.path.cmp(&right.path).then(left.kind.cmp(right.kind)));
        Ok(generated)
    }
}

#[derive(Default)]
struct CollectedTree {
    files: BTreeSet<PathBuf>,
    directories: BTreeSet<PathBuf>,
}

fn collect_tree(root: &Path) -> Result<CollectedTree, String> {
    let mut tree = CollectedTree::default();
    collect_tree_from(root, root, &mut tree)?;
    Ok(tree)
}

fn collect_tree_from(
    root: &Path,
    directory: &Path,
    tree: &mut CollectedTree,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("reading {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("reading {}: {error}", directory.display()))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("inspecting {}: {error}", entry.path().display()))?;
        if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
            return Err(format!(
                "coding workspace contains unsupported entry {}",
                entry.path().display()
            ));
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| "workspace traversal escaped its root".to_owned())?
            .to_path_buf();
        validate_tree_path(&relative)?;
        if kind.is_dir() {
            tree.directories.insert(relative);
            collect_tree_from(root, &entry.path(), tree)?;
        } else {
            tree.files.insert(relative);
        }
    }
    Ok(())
}

fn copy_regular_tree(root: &Path, directory: &Path, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("reading {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("reading {}: {error}", directory.display()))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("inspecting {}: {error}", entry.path().display()))?;
        if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
            return Err(format!(
                "coding workspace contains unsupported entry {}",
                entry.path().display()
            ));
        }
        let entry_path = entry.path();
        let relative = entry_path
            .strip_prefix(root)
            .map_err(|_| "workspace traversal escaped its root".to_owned())?;
        validate_tree_path(relative)?;
        let target = destination.join(relative);
        if kind.is_dir() {
            private_dir(&target)?;
            copy_regular_tree(root, &entry_path, destination)?;
        } else {
            fs::copy(&entry_path, &target).map_err(|error| {
                format!(
                    "copying reviewed source {} to {}: {error}",
                    entry_path.display(),
                    target.display()
                )
            })?;
            let permissions = entry
                .metadata()
                .map_err(|error| format!("inspecting {}: {error}", entry_path.display()))?
                .permissions();
            fs::set_permissions(&target, permissions)
                .map_err(|error| format!("securing {}: {error}", target.display()))?;
        }
    }
    Ok(())
}

fn files_equal_with_mode(left: &Path, right: &Path) -> Result<bool, String> {
    let left_metadata = left
        .symlink_metadata()
        .map_err(|error| format!("inspecting {}: {error}", left.display()))?;
    let right_metadata = right
        .symlink_metadata()
        .map_err(|error| format!("inspecting {}: {error}", right.display()))?;
    if !left_metadata.is_file()
        || !right_metadata.is_file()
        || left_metadata.permissions().mode() & 0o111 != right_metadata.permissions().mode() & 0o111
    {
        return Ok(false);
    }
    optional_files_equal(left, right)
}

fn directory_identity(path: &Path, label: &str) -> Result<DirectoryIdentity, String> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| format!("{label} {} is unavailable: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{label} {} is not a real directory",
            path.display()
        ));
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_directory_identity(
    path: &Path,
    expected: DirectoryIdentity,
    label: &str,
) -> Result<(), String> {
    let observed = directory_identity(path, label)?;
    if observed == expected {
        Ok(())
    } else {
        Err(format!("{label} {} was replaced", path.display()))
    }
}

fn utf8_path(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("generated output path {path:?} is not UTF-8"))
}

fn require_ignored_outputs(
    snapshot: &RepositorySnapshot,
    supervisor: &ProcessSupervisor,
    files: &[PathBuf],
) -> Result<(), String> {
    let mut input = Vec::new();
    for path in files {
        input.extend_from_slice(path.as_os_str().as_bytes());
        input.push(0);
    }
    if input.is_empty() {
        return Ok(());
    }
    const MAX_IGNORE_INPUT: usize = 16 * 1024 * 1024;
    if input.len() > MAX_IGNORE_INPUT {
        return Err("verification generated too many output path bytes to classify".to_owned());
    }
    let cancellation = CancellationToken::new();
    let runner = GitRunner::new(supervisor, &cancellation);
    let report = runner.run_with_stdin(
        &snapshot.source,
        &[
            OsStr::new("check-ignore"),
            OsStr::new("--no-index"),
            OsStr::new("-z"),
            OsStr::new("--stdin"),
        ],
        input,
    )?;
    let valid = matches!(report.termination, ProcessTermination::Exited(0 | 1))
        && report.cleanup_complete
        && report.infrastructure_failures.is_empty()
        && report.stdout_discarded_bytes == 0
        && report.stderr_discarded_bytes == 0
        && report.stderr.is_empty();
    if !valid {
        return Err(format!(
            "local Git ignore classification failed with {:?}: {}",
            report.termination,
            String::from_utf8_lossy(&report.stderr).trim()
        ));
    }
    let ignored: BTreeSet<&[u8]> = report
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    for path in files {
        if !ignored.contains(path.as_os_str().as_bytes()) {
            return Err(format!(
                "verification created non-ignored unchecked output {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn optional_files_equal(before: &Path, after: &Path) -> Result<bool, String> {
    let before_length = optional_file_length(before)?;
    let after_length = optional_file_length(after)?;
    let (Some(before_length), Some(after_length)) = (before_length, after_length) else {
        return Ok(before_length == after_length);
    };
    if before_length != after_length {
        return Ok(false);
    }

    let mut before_reader = BufReader::new(
        File::open(before).map_err(|error| format!("reading {}: {error}", before.display()))?,
    );
    let mut after_reader = BufReader::new(
        File::open(after).map_err(|error| format!("reading {}: {error}", after.display()))?,
    );
    loop {
        let before_buffer = before_reader
            .fill_buf()
            .map_err(|error| format!("reading {}: {error}", before.display()))?;
        let after_buffer = after_reader
            .fill_buf()
            .map_err(|error| format!("reading {}: {error}", after.display()))?;
        let compared = before_buffer.len().min(after_buffer.len());
        if before_buffer[..compared] != after_buffer[..compared] {
            return Ok(false);
        }
        if before_buffer.is_empty() || after_buffer.is_empty() {
            return Ok(before_buffer.is_empty() && after_buffer.is_empty());
        }
        before_reader.consume(compared);
        after_reader.consume(compared);
    }
}

fn optional_file_length(path: &Path) -> Result<Option<u64>, String> {
    match path.metadata() {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspecting {}: {error}", path.display())),
    }
}

fn read_file_for_context(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    match path.symlink_metadata() {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(format!(
                "changed source {} is not a regular file",
                path.display()
            ));
        }
        Ok(metadata) if usize::try_from(metadata.len()).map_or(true, |length| length > maximum) => {
            return Err(format!(
                "changed file {} exceeds the {maximum}-byte review bound",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(error) => return Err(format!("inspecting {}: {error}", path.display())),
    }
    fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))
}

fn append_context(
    output: &mut Vec<u8>,
    path: &Path,
    label: &str,
    bytes: &[u8],
    maximum: usize,
) -> Result<(), String> {
    use std::fmt::Write as _;
    std::str::from_utf8(bytes).map_err(|_| format!("changed source file {path:?} is not UTF-8"))?;
    let mut header = String::new();
    write!(
        &mut header,
        "\n--- {label} {path:?} ({} bytes) ---\n",
        bytes.len()
    )
    .map_err(|_| "formatting review context".to_owned())?;
    let footer = if bytes.ends_with(b"\n") {
        "--- end complete file contents ---\n"
    } else {
        "\n--- end complete file contents; no final newline ---\n"
    };
    let required = output
        .len()
        .checked_add(header.len())
        .and_then(|length| length.checked_add(bytes.len()))
        .and_then(|length| length.checked_add(footer.len()))
        .ok_or_else(|| "changed source context size overflow".to_owned())?;
    if required > maximum {
        return Err(format!(
            "changed source context exceeds the {maximum}-byte review bound"
        ));
    }
    output.extend_from_slice(header.as_bytes());
    output.extend_from_slice(bytes);
    output.extend_from_slice(footer.as_bytes());
    Ok(())
}

fn verify_source_state(runner: &GitRunner<'_>, source: &Path, head: &str) -> Result<(), String> {
    ensure_clean(runner, source)?;
    let after = runner
        .success_text(source, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_owned();
    if after != head {
        return Err(format!("source HEAD changed from {head} to {after}"));
    }
    Ok(())
}

fn validated_diff_output(report: ProcessReport) -> Result<Vec<u8>, String> {
    let complete = report.cleanup_complete
        && report.infrastructure_failures.is_empty()
        && report.stdout_discarded_bytes == 0
        && report.stderr_discarded_bytes == 0
        && report.stderr.is_empty();
    match report.termination {
        ProcessTermination::Exited(0) if complete && report.stdout.is_empty() => Ok(report.stdout),
        ProcessTermination::Exited(1) if complete && !report.stdout.is_empty() => Ok(report.stdout),
        _ => Err(format!(
            "local Git diff failed with {:?}: {}",
            report.termination,
            String::from_utf8_lossy(&report.stderr).trim()
        )),
    }
}

fn ensure_clean(runner: &GitRunner<'_>, source: &Path) -> Result<(), String> {
    let status = runner.success_bytes(
        source,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if status.is_empty() {
        Ok(())
    } else {
        Err("source repository is not clean, including untracked files".to_owned())
    }
}

fn read_blob_sizes(
    runner: &GitRunner<'_>,
    source: &Path,
    entries: &[TreeEntry],
) -> Result<Vec<u64>, String> {
    let mut sizes = Vec::with_capacity(entries.len());
    for page in entries.chunks(GIT_BATCH_ITEMS) {
        let report = runner.run_with_stdin(
            source,
            &[OsStr::new("cat-file"), OsStr::new("--batch-check")],
            batch_input(page),
        )?;
        let output = validated_git_stdout(report, "object-size batch")?;
        sizes.extend(parse_batch_check(&output, page)?);
    }
    Ok(sizes)
}

fn write_blob_batches(
    runner: &GitRunner<'_>,
    source: &Path,
    baseline: &Path,
    entries: &[TreeEntry],
    sizes: &[u64],
) -> Result<(), String> {
    if entries.len() != sizes.len() {
        return Err("internal snapshot object-size count mismatch".to_owned());
    }

    let mut start = 0;
    while start < entries.len() {
        match next_blob_batch(entries, sizes, start)? {
            BlobBatch::Unframed => {
                let entry = &entries[start];
                let bytes = runner.success_bytes(source, &["cat-file", "blob", &entry.oid])?;
                if u64::try_from(bytes.len()).ok() != Some(sizes[start]) {
                    return Err(format!(
                        "Git returned an inconsistent blob for {}",
                        entry.path.display()
                    ));
                }
                write_tree_file(baseline, entry, &bytes)?;
                start += 1;
            }
            BlobBatch::Framed { end, output_bytes } => {
                let page_entries = &entries[start..end];
                let page_sizes = &sizes[start..end];
                let report = runner.run_with_stdin(
                    source,
                    &[OsStr::new("cat-file"), OsStr::new("--batch")],
                    batch_input(page_entries),
                )?;
                let output = validated_git_stdout(report, "blob-content batch")?;
                if output.len() != output_bytes {
                    return Err("Git returned an incorrectly sized blob-content batch".to_owned());
                }
                let blobs = parse_blob_batch(&output, page_entries, page_sizes)?;
                for (entry, bytes) in page_entries.iter().zip(blobs) {
                    write_tree_file(baseline, entry, bytes)?;
                }
                start = end;
            }
        }
    }
    Ok(())
}

fn batch_input(entries: &[TreeEntry]) -> Vec<u8> {
    let capacity = entries.iter().map(|entry| entry.oid.len() + 1).sum();
    let mut input = Vec::with_capacity(capacity);
    for entry in entries {
        input.extend_from_slice(entry.oid.as_bytes());
        input.push(b'\n');
    }
    input
}

fn validated_git_stdout(report: ProcessReport, operation: &str) -> Result<Vec<u8>, String> {
    if report.is_success()
        && report.cleanup_complete
        && report.infrastructure_failures.is_empty()
        && report.stdout_discarded_bytes == 0
        && report.stderr_discarded_bytes == 0
        && report.stdout.len() <= GIT_MAX_OUTPUT
    {
        Ok(report.stdout)
    } else {
        Err(format!(
            "local Git {operation} failed with {:?}: {}",
            report.termination,
            String::from_utf8_lossy(&report.stderr).trim()
        ))
    }
}

fn parse_batch_check(output: &[u8], expected: &[TreeEntry]) -> Result<Vec<u64>, String> {
    if expected.is_empty() {
        return if output.is_empty() {
            Ok(Vec::new())
        } else {
            Err("Git returned unexpected object-size records".to_owned())
        };
    }
    let records = output
        .strip_suffix(b"\n")
        .ok_or_else(|| "Git returned a truncated object-size batch".to_owned())?;
    let mut lines = records.split(|byte| *byte == b'\n');
    let mut sizes = Vec::with_capacity(expected.len());
    for entry in expected {
        let line = lines
            .next()
            .ok_or_else(|| "Git returned too few object-size records".to_owned())?;
        let (oid, kind, raw_size) = parse_batch_header(line)?;
        if oid != entry.oid.as_bytes() {
            return Err(format!(
                "Git returned an unexpected object id for {}",
                entry.path.display()
            ));
        }
        if kind != b"blob" {
            return Err(format!(
                "Git returned a non-blob object for {}",
                entry.path.display()
            ));
        }
        let size = parse_decimal(raw_size)?;
        if size > MAX_BLOB_BYTES {
            return Err(format!(
                "tracked file {} exceeds the {MAX_BLOB_BYTES}-byte snapshot limit",
                entry.path.display()
            ));
        }
        sizes.push(size);
    }
    if lines.next().is_some() {
        return Err("Git returned too many object-size records".to_owned());
    }
    Ok(sizes)
}

fn parse_blob_batch<'a>(
    output: &'a [u8],
    expected: &[TreeEntry],
    sizes: &[u64],
) -> Result<Vec<&'a [u8]>, String> {
    if expected.len() != sizes.len() {
        return Err("internal blob-content batch size mismatch".to_owned());
    }
    let mut offset = 0;
    let mut blobs = Vec::with_capacity(expected.len());
    for (entry, expected_size) in expected.iter().zip(sizes) {
        let header_end = output[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|relative| offset + relative)
            .ok_or_else(|| "Git returned a truncated blob-content header".to_owned())?;
        let (oid, kind, raw_size) = parse_batch_header(&output[offset..header_end])?;
        if oid != entry.oid.as_bytes() {
            return Err(format!(
                "Git returned an unexpected object id for {}",
                entry.path.display()
            ));
        }
        if kind != b"blob" {
            return Err(format!(
                "Git returned a non-blob object for {}",
                entry.path.display()
            ));
        }
        let reported_size = parse_decimal(raw_size)?;
        if reported_size != *expected_size {
            return Err(format!(
                "Git returned an inconsistent blob size for {}",
                entry.path.display()
            ));
        }
        let content_start = header_end + 1;
        let content_length = usize::try_from(*expected_size)
            .map_err(|_| "Git blob size does not fit this platform".to_owned())?;
        let content_end = content_start
            .checked_add(content_length)
            .ok_or_else(|| "Git blob frame size overflowed".to_owned())?;
        if content_end >= output.len() || output[content_end] != b'\n' {
            return Err(format!(
                "Git returned a truncated or malformed blob for {}",
                entry.path.display()
            ));
        }
        blobs.push(&output[content_start..content_end]);
        offset = content_end + 1;
    }
    if offset != output.len() {
        return Err("Git returned unexpected extra blob-content data".to_owned());
    }
    Ok(blobs)
}

type BatchHeader<'a> = (&'a [u8], &'a [u8], &'a [u8]);

fn parse_batch_header(line: &[u8]) -> Result<BatchHeader<'_>, String> {
    let mut fields = line.split(|byte| *byte == b' ');
    let oid = fields
        .next()
        .ok_or_else(|| "Git returned a malformed batch record".to_owned())?;
    let kind = fields
        .next()
        .ok_or_else(|| "Git returned a malformed batch record".to_owned())?;
    let size = fields
        .next()
        .ok_or_else(|| "Git returned a malformed batch record".to_owned())?;
    if oid.is_empty() || kind.is_empty() || size.is_empty() || fields.next().is_some() {
        return Err("Git returned a malformed batch record".to_owned());
    }
    Ok((oid, kind, size))
}

fn parse_decimal(raw: &[u8]) -> Result<u64, String> {
    if raw.is_empty()
        || (raw.len() > 1 && raw[0] == b'0')
        || raw.iter().any(|byte| !byte.is_ascii_digit())
    {
        return Err("Git returned a malformed decimal object size".to_owned());
    }
    raw.iter().try_fold(0_u64, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(*byte - b'0')))
            .ok_or_else(|| "Git object size overflowed".to_owned())
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlobBatch {
    Framed { end: usize, output_bytes: usize },
    Unframed,
}

fn next_blob_batch(
    entries: &[TreeEntry],
    sizes: &[u64],
    start: usize,
) -> Result<BlobBatch, String> {
    let first = entries
        .get(start)
        .ok_or_else(|| "internal blob batch start is out of range".to_owned())?;
    let first_size = *sizes
        .get(start)
        .ok_or_else(|| "internal blob batch size is missing".to_owned())?;
    let first_frame = blob_frame_bytes(&first.oid, first_size)?;
    if first_frame > GIT_MAX_OUTPUT as u64 {
        return Ok(BlobBatch::Unframed);
    }

    let mut output_bytes = 0_u64;
    let mut end = start;
    while end < entries.len() && end - start < GIT_BATCH_ITEMS {
        let size = *sizes
            .get(end)
            .ok_or_else(|| "internal blob batch size is missing".to_owned())?;
        let frame = blob_frame_bytes(&entries[end].oid, size)?;
        let Some(combined) = output_bytes.checked_add(frame) else {
            break;
        };
        if combined > GIT_MAX_OUTPUT as u64 {
            break;
        }
        output_bytes = combined;
        end += 1;
    }
    Ok(BlobBatch::Framed {
        end,
        output_bytes: usize::try_from(output_bytes)
            .map_err(|_| "blob-content batch does not fit this platform".to_owned())?,
    })
}

fn blob_frame_bytes(oid: &str, size: u64) -> Result<u64, String> {
    let oid_bytes = u64::try_from(oid.len()).map_err(|_| "object id is too long".to_owned())?;
    oid_bytes
        .checked_add(8)
        .and_then(|bytes| bytes.checked_add(decimal_digits(size)))
        .and_then(|bytes| bytes.checked_add(size))
        .ok_or_else(|| "Git blob frame size overflowed".to_owned())
}

fn decimal_digits(mut value: u64) -> u64 {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn parse_tree(bytes: &[u8]) -> Result<Vec<TreeEntry>, String> {
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let text = std::str::from_utf8(record)
            .map_err(|_| "tracked paths and tree records must be UTF-8".to_owned())?;
        let (header, raw_path) = text
            .split_once('\t')
            .ok_or_else(|| "Git emitted a malformed tree record".to_owned())?;
        let mut fields = header.split(' ');
        let mode = fields
            .next()
            .ok_or_else(|| "tree mode missing".to_owned())?;
        let kind = fields
            .next()
            .ok_or_else(|| "tree type missing".to_owned())?;
        let oid = fields
            .next()
            .ok_or_else(|| "tree object id missing".to_owned())?;
        if fields.next().is_some() || kind != "blob" || !matches!(mode, "100644" | "100755") {
            return Err(format!(
                "unsupported tracked entry {raw_path:?} with mode {mode} and type {kind}; symlinks and submodules are refused"
            ));
        }
        validate_object_id(oid)?;
        let path = PathBuf::from(raw_path);
        validate_tree_path(&path)?;
        entries.push(TreeEntry {
            path,
            oid: oid.to_owned(),
            executable: mode == "100755",
        });
    }
    Ok(entries)
}

fn validate_object_id(oid: &str) -> Result<(), String> {
    if !matches!(oid.len(), 40 | 64)
        || oid
            .as_bytes()
            .iter()
            .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("Git emitted an invalid full object id".to_owned());
    }
    Ok(())
}

fn validate_tree_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(format!(
            "tracked path {:?} is not a confined relative path",
            path
        ));
    }
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(format!("tracked path {:?} can escape the snapshot", path));
        };
        if name
            .to_str()
            .is_some_and(|part| part.eq_ignore_ascii_case(".git"))
        {
            return Err(format!("tracked path {:?} impersonates Git metadata", path));
        }
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), String> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|error| format!("creating {}: {error}", path.display()))
}

fn write_tree_file(root: &Path, entry: &TreeEntry, bytes: &[u8]) -> Result<(), String> {
    let path = root.join(&entry.path);
    let parent = path
        .parent()
        .ok_or_else(|| format!("tracked path {} has no parent", entry.path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("securing {}: {error}", parent.display()))?;
    let mode = if entry.executable { 0o700 } else { 0o600 };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&path)
        .map_err(|error| format!("creating {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("writing {}: {error}", path.display()))
}

fn copy_tree(baseline: &Path, workspace: &Path, entries: &[TreeEntry]) -> Result<(), String> {
    for entry in entries {
        let source = baseline.join(&entry.path);
        let destination = workspace.join(&entry.path);
        let parent = destination
            .parent()
            .ok_or_else(|| format!("tracked path {} has no parent", entry.path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("securing {}: {error}", parent.display()))?;
        fs::copy(&source, &destination).map_err(|error| {
            format!(
                "copying {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
        let mode = if entry.executable { 0o700 } else { 0o600 };
        fs::set_permissions(&destination, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("securing {}: {error}", destination.display()))?;
    }
    Ok(())
}

struct GitRunner<'a> {
    supervisor: &'a ProcessSupervisor,
    cancellation: &'a CancellationToken,
}

impl<'a> GitRunner<'a> {
    fn new(supervisor: &'a ProcessSupervisor, cancellation: &'a CancellationToken) -> Self {
        Self {
            supervisor,
            cancellation,
        }
    }

    fn success_text(&self, cwd: &Path, arguments: &[&str]) -> Result<String, String> {
        let bytes = self.success_bytes(cwd, arguments)?;
        String::from_utf8(bytes).map_err(|_| "local Git output was not UTF-8".to_owned())
    }

    fn success_bytes(&self, cwd: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
        let arguments: Vec<&OsStr> = arguments.iter().map(OsStr::new).collect();
        let report = self.run(cwd, &arguments)?;
        if report.is_success()
            && report.cleanup_complete
            && report.infrastructure_failures.is_empty()
            && report.stdout_discarded_bytes == 0
            && report.stderr_discarded_bytes == 0
        {
            Ok(report.stdout)
        } else {
            Err(format!(
                "local Git failed with {:?}: {}",
                report.termination,
                String::from_utf8_lossy(&report.stderr).trim()
            ))
        }
    }

    fn run(&self, cwd: &Path, arguments: &[&OsStr]) -> Result<ProcessReport, String> {
        self.run_inner(cwd, arguments, None)
    }

    fn run_with_stdin(
        &self,
        cwd: &Path,
        arguments: &[&OsStr],
        stdin: Vec<u8>,
    ) -> Result<ProcessReport, String> {
        self.run_inner(cwd, arguments, Some(stdin))
    }

    fn run_inner(
        &self,
        cwd: &Path,
        arguments: &[&OsStr],
        stdin: Option<Vec<u8>>,
    ) -> Result<ProcessReport, String> {
        let mut argv = vec![
            OsString::from("git"),
            OsString::from("-c"),
            OsString::from("core.hooksPath=/dev/null"),
            OsString::from("-c"),
            OsString::from("credential.helper="),
            OsString::from("-c"),
            OsString::from("core.fsmonitor=false"),
        ];
        argv.extend(arguments.iter().map(|argument| (*argument).to_owned()));
        let mut spec = ProcessSpec::new(argv, GIT_TIMEOUT)
            .map_err(|error| format!("building local Git request: {error}"))?
            .with_max_output_bytes(GIT_MAX_OUTPUT)
            .with_cancellation(self.cancellation.clone())
            .with_inherited_env("PATH")
            .with_inherited_env("TMPDIR")
            .with_env("GIT_CONFIG_NOSYSTEM", "1")
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_TERMINAL_PROMPT", "0")
            .with_env("GIT_OPTIONAL_LOCKS", "0")
            .with_env("GIT_LFS_SKIP_SMUDGE", "1");
        if let Some(stdin) = stdin {
            spec = spec.with_stdin(stdin);
        }
        self.supervisor
            .run(&spec, cwd)
            .map_err(|error| format!("running local Git: {error}"))
    }
}

#[cfg(test)]
mod snapshot_tests {
    use std::error::Error;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn entry(path: &str, oid: &str) -> TreeEntry {
        TreeEntry {
            path: PathBuf::from(path),
            oid: oid.to_owned(),
            executable: false,
        }
    }

    fn git(repo: &Path, arguments: &[&str]) -> Result<(), Box<dyn Error>> {
        let output = Command::new("git")
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(arguments)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "git {arguments:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(())
    }

    #[test]
    fn batch_check_requires_exact_records_but_allows_requested_duplicates()
    -> Result<(), Box<dyn Error>> {
        let first = entry("first", OID_A);
        let second = entry("second", OID_B);
        let expected = [first.clone(), second.clone()];
        let valid = format!("{OID_A} blob 4\n{OID_B} blob 0\n");
        assert_eq!(parse_batch_check(valid.as_bytes(), &expected)?, [4, 0]);

        let repeated = [first.clone(), entry("shared", OID_A)];
        let repeated_output = format!("{OID_A} blob 4\n{OID_A} blob 4\n");
        assert_eq!(
            parse_batch_check(repeated_output.as_bytes(), &repeated)?,
            [4, 4]
        );

        let malformed = [
            format!("{OID_B} blob 4\n{OID_B} blob 0\n"),
            format!("{OID_A} tree 4\n{OID_B} blob 0\n"),
            format!("{OID_A} blob 4\n{OID_A} blob 0\n"),
            format!("{OID_A} blob 04\n{OID_B} blob 0\n"),
            format!("{OID_A} blob 4\n"),
            format!("{OID_A} blob 4\n{OID_B} blob 0\n{OID_A} blob 4\n"),
            format!("{OID_A} blob 4\n{OID_B} blob 0"),
        ];
        for output in malformed {
            assert!(parse_batch_check(output.as_bytes(), &expected).is_err());
        }
        Ok(())
    }

    #[test]
    fn blob_batch_parser_preserves_binary_and_empty_frames() -> Result<(), Box<dyn Error>> {
        let expected = [entry("binary", OID_A), entry("empty", OID_B)];
        let binary = b"a\n\0z";
        let mut output = format!("{OID_A} blob {}\n", binary.len()).into_bytes();
        output.extend_from_slice(binary);
        output.push(b'\n');
        output.extend_from_slice(format!("{OID_B} blob 0\n\n").as_bytes());

        let parsed = parse_blob_batch(&output, &expected, &[4, 0])?;

        assert_eq!(parsed[0], binary);
        assert!(parsed[1].is_empty());
        Ok(())
    }

    #[test]
    fn blob_batch_parser_rejects_wrong_missing_truncated_and_extra_frames() {
        let expected = [entry("binary", OID_A)];
        let malformed = [
            format!("{OID_B} blob 4\ndata\n").into_bytes(),
            format!("{OID_A} tree 4\ndata\n").into_bytes(),
            format!("{OID_A} blob 3\ndata\n").into_bytes(),
            format!("{OID_A} blob 4\ndat").into_bytes(),
            format!("{OID_A} blob 4\ndata").into_bytes(),
            format!("{OID_A} blob 4\ndata\nextra").into_bytes(),
            Vec::new(),
        ];
        for output in malformed {
            assert!(parse_blob_batch(&output, &expected, &[4]).is_err());
        }
    }

    #[test]
    fn blob_batches_include_framing_in_the_output_cap_and_accept_the_blob_limit()
    -> Result<(), Box<dyn Error>> {
        let near_limit = GIT_MAX_OUTPUT as u64
            - u64::try_from(OID_A.len())?
            - 8
            - decimal_digits(GIT_MAX_OUTPUT as u64);
        assert_eq!(blob_frame_bytes(OID_A, near_limit)?, GIT_MAX_OUTPUT as u64);
        let entries = [entry("large", OID_A), entry("empty", OID_B)];
        assert_eq!(
            next_blob_batch(&entries, &[near_limit, 0], 0)?,
            BlobBatch::Framed {
                end: 1,
                output_bytes: GIT_MAX_OUTPUT,
            }
        );
        assert_eq!(
            next_blob_batch(&entries, &[MAX_BLOB_BYTES, 0], 0)?,
            BlobBatch::Unframed
        );

        let at_limit = format!("{OID_A} blob {MAX_BLOB_BYTES}\n");
        assert_eq!(
            parse_batch_check(at_limit.as_bytes(), &entries[..1])?,
            [MAX_BLOB_BYTES]
        );
        let over_limit = format!("{OID_A} blob {}\n", MAX_BLOB_BYTES + 1);
        assert!(parse_batch_check(over_limit.as_bytes(), &entries[..1]).is_err());
        Ok(())
    }

    #[test]
    fn blob_batches_are_bounded_by_item_count() -> Result<(), Box<dyn Error>> {
        let entries: Vec<TreeEntry> = (0..=GIT_BATCH_ITEMS)
            .map(|index| entry(&format!("file-{index}"), OID_A))
            .collect();
        let sizes = vec![0; entries.len()];
        assert!(matches!(
            next_blob_batch(&entries, &sizes, 0)?,
            BlobBatch::Framed { end, .. } if end == GIT_BATCH_ITEMS
        ));
        Ok(())
    }

    #[test]
    fn tree_parser_rejects_non_full_or_protocol_like_object_ids() {
        for oid in ["HEAD", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:g"] {
            let record = format!("100644 blob {oid}\tfile\0");
            assert!(parse_tree(record.as_bytes()).is_err());
        }
        let uppercase = format!("100644 blob {}\tfile\0", OID_A.to_uppercase());
        assert!(parse_tree(uppercase.as_bytes()).is_err());
    }

    #[test]
    fn actual_git_snapshot_batches_many_files_and_preserves_modes() -> Result<(), Box<dyn Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "nanika-snapshot-batches-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private_dir(&root)?;
        let repo = root.join("repo");
        private_dir(&repo)?;
        git(&repo, &["init", "-q"])?;
        git(&repo, &["config", "user.name", "Snapshot Fixture"])?;
        git(&repo, &["config", "user.email", "snapshot@example.invalid"])?;
        git(&repo, &["config", "core.fileMode", "true"])?;
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "empty"])?;

        let supervisor = ProcessSupervisor::process_wide()?;
        let cancellation = CancellationToken::new();
        let empty_output = root.join("empty-output");
        private_dir(&empty_output)?;
        let empty = prepare(&repo, &empty_output, &supervisor, &cancellation)?;
        assert_eq!(fs::read_dir(&empty.baseline)?.count(), 0);
        assert_eq!(fs::read_dir(&empty.workspace)?.count(), 0);

        for index in 0..(GIT_BATCH_ITEMS * 2 + 3) {
            fs::write(
                repo.join(format!("file-{index:03}.txt")),
                format!("{index}\n"),
            )?;
        }
        fs::write(repo.join("shared-a.bin"), b"same\n\0bytes")?;
        fs::write(repo.join("shared-b.bin"), b"same\n\0bytes")?;
        fs::write(repo.join("empty.bin"), b"")?;
        let executable = repo.join("run.sh");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
        git(&repo, &["add", "."])?;
        git(&repo, &["commit", "-q", "-m", "many files"])?;

        let many_output = root.join("many-output");
        private_dir(&many_output)?;
        let snapshot = prepare(&repo, &many_output, &supervisor, &cancellation)?;
        for index in 0..(GIT_BATCH_ITEMS * 2 + 3) {
            let relative = format!("file-{index:03}.txt");
            let expected = format!("{index}\n").into_bytes();
            assert_eq!(fs::read(snapshot.baseline.join(&relative))?, expected);
            assert_eq!(fs::read(snapshot.workspace.join(&relative))?, expected);
        }
        assert_eq!(
            fs::read(snapshot.baseline.join("shared-a.bin"))?,
            b"same\n\0bytes"
        );
        assert_eq!(
            fs::read(snapshot.baseline.join("shared-b.bin"))?,
            b"same\n\0bytes"
        );
        assert_eq!(fs::read(snapshot.baseline.join("empty.bin"))?, b"");
        assert_ne!(
            fs::metadata(snapshot.baseline.join("run.sh"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_ne!(
            fs::metadata(snapshot.workspace.join("run.sh"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_eq!(
            fs::metadata(snapshot.baseline.join("file-000.txt"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_eq!(
            fs::metadata(snapshot.workspace.join("file-000.txt"))?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn actual_git_missing_root_error_is_not_an_empty_diff() -> Result<(), Box<dyn Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "nanika-snapshot-git-error-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private_dir(&root)?;
        private_dir(&root.join("baseline"))?;
        let supervisor = ProcessSupervisor::process_wide()?;
        let cancellation = CancellationToken::new();
        let runner = GitRunner::new(&supervisor, &cancellation);
        let report = runner.run(
            &root,
            &[
                OsStr::new("diff"),
                OsStr::new("--no-index"),
                OsStr::new("--"),
                OsStr::new("baseline"),
                OsStr::new("missing-workspace"),
            ],
        )?;
        assert!(matches!(report.termination, ProcessTermination::Exited(1)));
        assert!(report.stdout.is_empty());
        assert!(!report.stderr.is_empty());
        assert!(validated_diff_output(report).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
