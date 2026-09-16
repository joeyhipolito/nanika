//! End-to-end proof for B4-DESIGN §§2, 3 and 5, per the §6.1 test plan.
//!
//! Every repository lives under a per-case fixture root that is deleted on
//! `Drop`, and every remote is a local bare repository this file creates. The
//! file is an offline artifact by construction: no case names a network URL
//! except to prove the library refuses one, and the only pull-request path
//! exercised is [`FixturePrAdapter`], which contacts nothing.
//!
//! `no_network_and_no_pr_provider_is_contacted` makes that claim observable
//! rather than asserted. It re-enters this same test binary with `PATH`
//! pointing at a shim directory whose `git` records every argv before exec'ing
//! the real binary and whose `gh` writes a marker file. The parent then reads
//! the recording. `PATH` is set on the *child* process, which is safe; the
//! test never mutates its own environment (`std::env::set_var` is `unsafe`, and
//! this workspace forbids unsafe code).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use orchestrator_git::{
    CommitSha, ConfinementRefusal, FixturePrAdapter, GitError, LOCK_FILE_NAME, PrAdapter,
    PrAdapterError, PrRequest, PushAck, RefreshedBase, RemotePolicy, TRASH_META_FILE_NAME,
    branch_name, claim_changed_files, commit_all, create_branch_from_refreshed_base,
    create_worktree, head_sha, ls_remote_branch, open_claims_db, push_with_acknowledgement,
    remove_lock, remove_worktree_confined, resolve_refreshed_base, write_lock,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

/// A fixture root. Everything a case creates lives under it, and it is the
/// confinement root every cleanup call is given.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "orchestrator-git-refreshed-base-{}-{number}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Resolves the real `git` binary by scanning `PATH`.
///
/// Fixture setup uses this absolute path rather than the name `git`, so that
/// under the recording shim only the *library's* invocations are recorded.
fn resolve_real_git() -> TestResult<PathBuf> {
    let path = std::env::var_os("PATH").ok_or("PATH is unset")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("git");
        if candidate.is_file() && !candidate.starts_with(std::env::temp_dir()) {
            return Ok(candidate);
        }
    }
    Err("no git binary on PATH".into())
}

fn run_git(git: &Path, dir: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new(git)
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("git {args:?} in {} failed: {combined}", dir.display()).into());
    }
    Ok(combined.trim().to_owned())
}

/// Creates an empty bare repository at `path` with `main` as its HEAD.
fn init_bare(git: &Path, path: &Path) -> TestResult {
    std::fs::create_dir_all(path)?;
    run_git(git, path, &["init", "--bare", "-b", "main"])?;
    Ok(())
}

/// Clones `remote` to `into` and gives it a commit identity.
fn clone_repo(git: &Path, remote: &Path, into: &Path) -> TestResult {
    let parent = into.parent().ok_or("clone target has no parent")?;
    std::fs::create_dir_all(parent)?;
    run_git(
        git,
        parent,
        &["clone", &remote.to_string_lossy(), &into.to_string_lossy()],
    )?;
    run_git(git, into, &["config", "user.email", "test@example.com"])?;
    run_git(git, into, &["config", "user.name", "Test"])?;
    Ok(())
}

/// Adds a commit named `label` in `dir` and returns its sha.
fn commit_file(git: &Path, dir: &Path, label: &str) -> TestResult<CommitSha> {
    std::fs::write(dir.join(format!("{label}.txt")), format!("{label}\n"))?;
    run_git(git, dir, &["add", "-A"])?;
    run_git(git, dir, &["commit", "-m", label])?;
    let sha = run_git(git, dir, &["rev-parse", "HEAD"])?;
    CommitSha::parse(&sha).ok_or_else(|| format!("not a full sha: {sha}").into())
}

/// Seeds a bare remote with one commit on `main`, via a throwaway clone.
fn seed_bare_remote(git: &Path, root: &Path, bare: &Path, label: &str) -> TestResult<CommitSha> {
    init_bare(git, bare)?;
    let seed = root.join(format!("seed-{label}"));
    run_git(
        git,
        root,
        &["clone", &bare.to_string_lossy(), &seed.to_string_lossy()],
    )?;
    run_git(git, &seed, &["config", "user.email", "test@example.com"])?;
    run_git(git, &seed, &["config", "user.name", "Test"])?;
    let sha = commit_file(git, &seed, label)?;
    run_git(git, &seed, &["push", "origin", "main"])?;
    std::fs::remove_dir_all(&seed)?;
    Ok(sha)
}

fn ref_exists(git: &Path, repo: &Path, reference: &str) -> TestResult<bool> {
    Ok(!run_git(
        git,
        repo,
        &["for-each-ref", "--format=%(refname)", reference],
    )?
    .is_empty())
}

// ---------------------------------------------------------------------------
// §6.1 case 1
// ---------------------------------------------------------------------------

#[test]
fn fetch_before_branch_selects_advanced_remote_over_stale_local_main() -> TestResult {
    let s = Scratch::new("advanced")?;
    let git = resolve_real_git()?;
    let origin = s.root.join("origin.git");
    let stale_sha = seed_bare_remote(&git, &s.root, &origin, "base")?;

    // `work` clones at the seeded commit; its local main is now frozen there.
    let work = s.root.join("work");
    clone_repo(&git, &origin, &work)?;
    let work_local_main = run_git(&git, &work, &["rev-parse", "refs/heads/main"])?;
    assert_eq!(work_local_main, stale_sha.as_str());

    // A second clone advances origin/main behind `work`'s back — exactly the
    // TRK-1116 shape.
    let publisher = s.root.join("publisher");
    clone_repo(&git, &origin, &publisher)?;
    let advanced = commit_file(&git, &publisher, "advance")?;
    run_git(&git, &publisher, &["push", "origin", "main"])?;
    assert_ne!(advanced, stale_sha);

    let base = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly)?;
    assert_eq!(
        base.base_sha(),
        &advanced,
        "the fetched remote head must be selected"
    );
    assert_ne!(
        base.base_sha().as_str(),
        work_local_main,
        "the stale local main must not be selected"
    );
    assert_eq!(base.local_sha(), Some(&stale_sha));
    assert!(base.local_base_is_stale());

    let branch = branch_name("mission-advanced", "do the thing");
    create_branch_from_refreshed_base(&work, &branch, &base)?;
    let branch_head = run_git(&git, &work, &["rev-parse", &format!("refs/heads/{branch}")])?;
    assert_eq!(
        branch_head,
        advanced.as_str(),
        "the isolation branch must sit on the refreshed remote head"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// §6.1 case 2
// ---------------------------------------------------------------------------

#[test]
fn two_bare_remotes_prove_the_configured_remote_is_fetched() -> TestResult {
    let s = Scratch::new("tworemotes")?;
    let git = resolve_real_git()?;

    let alpha_bare = s.root.join("alpha.git");
    let alpha_head = seed_bare_remote(&git, &s.root, &alpha_bare, "alpha")?;
    let beta_bare = s.root.join("beta.git");
    let beta_head = seed_bare_remote(&git, &s.root, &beta_bare, "beta")?;
    assert_ne!(alpha_head, beta_head);

    let work = s.root.join("work");
    clone_repo(&git, &alpha_bare, &work)?;
    run_git(
        &git,
        &work,
        &["remote", "add", "alpha", &alpha_bare.to_string_lossy()],
    )?;
    run_git(
        &git,
        &work,
        &["remote", "add", "beta", &beta_bare.to_string_lossy()],
    )?;

    let beta = resolve_refreshed_base(&work, "beta", "main", RemotePolicy::LocalOnly)?;
    assert_eq!(beta.base_sha(), &beta_head);
    assert_ne!(beta.base_sha(), &alpha_head);
    assert_eq!(beta.remote(), "beta");

    let alpha = resolve_refreshed_base(&work, "alpha", "main", RemotePolicy::LocalOnly)?;
    assert_eq!(alpha.base_sha(), &alpha_head);
    assert_ne!(alpha.base_sha(), &beta_head);
    Ok(())
}

// ---------------------------------------------------------------------------
// §6.1 case 3
// ---------------------------------------------------------------------------

#[test]
fn refresh_refuses_when_the_remote_is_unreachable() -> TestResult {
    let s = Scratch::new("unreachable")?;
    let git = resolve_real_git()?;
    let origin = s.root.join("origin.git");
    seed_bare_remote(&git, &s.root, &origin, "base")?;
    let work = s.root.join("work");
    clone_repo(&git, &origin, &work)?;

    // The remote is still configured, but no longer exists.
    std::fs::remove_dir_all(&origin)?;

    let result = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly);
    assert!(
        matches!(result, Err(GitError::RemoteUnreachable { .. })),
        "expected RemoteUnreachable, got {result:?}"
    );
    assert!(
        !ref_exists(&git, &work, "refs/heads/via")?,
        "no isolation branch may be created when the base cannot be refreshed"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// §6.1 case 4
// ---------------------------------------------------------------------------

#[test]
fn refresh_refuses_a_non_file_remote_under_local_only() -> TestResult {
    let s = Scratch::new("nonlocal")?;
    let git = resolve_real_git()?;
    let origin = s.root.join("origin.git");
    seed_bare_remote(&git, &s.root, &origin, "base")?;
    let work = s.root.join("work");
    clone_repo(&git, &origin, &work)?;
    run_git(
        &git,
        &work,
        &["remote", "add", "upstream", "https://example.invalid/x.git"],
    )?;

    let result = resolve_refreshed_base(&work, "upstream", "main", RemotePolicy::LocalOnly);
    assert!(
        matches!(result, Err(GitError::RemoteNotLocal { .. })),
        "expected RemoteNotLocal, got {result:?}"
    );
    // No fetch was attempted: a fetch writes FETCH_HEAD and the tracking ref
    // even when it later fails, and neither exists.
    assert!(
        !work.join(".git/FETCH_HEAD").exists(),
        "a non-local remote must be refused before any fetch"
    );
    assert!(!ref_exists(&git, &work, "refs/remotes/upstream")?);

    // The scp-like shorthand is refused on the same path.
    run_git(
        &git,
        &work,
        &["remote", "add", "scp", "git@example.invalid:owner/x.git"],
    )?;
    assert!(matches!(
        resolve_refreshed_base(&work, "scp", "main", RemotePolicy::LocalOnly),
        Err(GitError::RemoteNotLocal { .. })
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// §6.1 case 5
// ---------------------------------------------------------------------------

#[test]
fn base_sha_is_recorded_before_any_branch_exists() -> TestResult {
    let s = Scratch::new("recordfirst")?;
    let git = resolve_real_git()?;
    let origin = s.root.join("origin.git");
    let origin_head = seed_bare_remote(&git, &s.root, &origin, "base")?;
    let work = s.root.join("work");
    clone_repo(&git, &origin, &work)?;

    let branch = branch_name("mission-record", "record before branch");
    let base = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly)?;

    // At the instant the base is known, no isolation branch exists yet, while
    // the recorded sha is already a real commit in the remote.
    assert!(
        !ref_exists(&git, &work, &format!("refs/heads/{branch}"))?,
        "the base must be resolvable before any branch is created"
    );
    assert_eq!(base.base_sha(), &origin_head);
    assert_eq!(
        run_git(
            &git,
            &origin,
            &["rev-parse", &format!("{}^{{commit}}", base.base_sha())]
        )?,
        origin_head.as_str()
    );

    create_branch_from_refreshed_base(&work, &branch, &base)?;
    assert_eq!(
        run_git(&git, &work, &["rev-parse", &format!("refs/heads/{branch}")])?,
        origin_head.as_str()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// §6.1 case 6
// ---------------------------------------------------------------------------

/// The isolation a mission holds between `CreateIsolation` and cleanup.
struct Isolation {
    worktree: PathBuf,
    claimed: Vec<String>,
}

/// Establishes branch + worktree + lock + claims off a refreshed base.
fn establish_isolation(
    repo_root: &Path,
    claims_db: &Path,
    mission_id: &str,
    branch: &str,
    base: &RefreshedBase,
) -> TestResult<Isolation> {
    create_branch_from_refreshed_base(repo_root, branch, base)?;
    let worktree = repo_root.join("wt");
    create_worktree(repo_root, &worktree, branch)?;
    write_lock(&worktree, mission_id)?;
    std::fs::write(worktree.join("work.txt"), "in progress\n")?;

    let claimed = claim_changed_files(
        Some(repo_root),
        Some(&worktree),
        Some(base.base_branch()),
        Some(branch),
    )?;
    let mut db = open_claims_db(Some(claims_db))?;
    db.claim_files(mission_id, &repo_root.to_string_lossy(), &claimed)?;
    db.close()?;
    Ok(Isolation { worktree, claimed })
}

/// The compensation an injected failure must trigger: release the claims, drop
/// the liveness lock, then trash the worktree inside the confinement root.
fn compensate(
    isolation: &Isolation,
    claims_db: &Path,
    mission_id: &str,
    trash_dir: &Path,
    confinement_root: &Path,
) -> TestResult<PathBuf> {
    let db = open_claims_db(Some(claims_db))?;
    db.release_all(mission_id)?;
    db.close()?;
    remove_lock(&isolation.worktree);
    Ok(remove_worktree_confined(
        &isolation.worktree,
        trash_dir,
        confinement_root,
    )?)
}

#[test]
fn injected_failure_leaves_no_claim_worktree_or_lock() -> TestResult {
    let s = Scratch::new("rollback")?;
    let git = resolve_real_git()?;
    let origin = s.root.join("origin.git");
    seed_bare_remote(&git, &s.root, &origin, "base")?;
    let work = s.root.join("work");
    clone_repo(&git, &origin, &work)?;

    let mission_id = "mission-rollback";
    let branch = branch_name(mission_id, "inject a failure");
    let claims_db = s.root.join("claims.db");
    let base = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly)?;
    let isolation = establish_isolation(&work, &claims_db, mission_id, &branch, &base)?;

    assert!(isolation.worktree.join(LOCK_FILE_NAME).exists());
    assert!(
        !isolation.claimed.is_empty(),
        "the fixture must claim files"
    );
    let observer = open_claims_db(Some(&claims_db))?;
    assert!(
        !observer
            .check_conflicts("other-mission", &work.to_string_lossy(), &isolation.claimed)?
            .is_empty(),
        "the control: while the mission holds them, the claims are visible"
    );
    observer.close()?;

    // --- injected failure, immediately after the worktree exists -------------
    let trash = s.root.join("trash");
    let trash_entry = compensate(&isolation, &claims_db, mission_id, &trash, &s.root)?;

    assert!(
        !isolation.worktree.exists(),
        "the worktree path must be gone"
    );
    let registered = run_git(&git, &work, &["worktree", "list", "--porcelain"])?;
    assert!(
        !registered.contains(&isolation.worktree.to_string_lossy().into_owned()),
        "no worktree registration may survive: {registered}"
    );

    let entries: Vec<_> = std::fs::read_dir(&trash)?.collect::<Result<_, _>>()?;
    assert_eq!(entries.len(), 1, "exactly one trash entry is expected");
    assert!(trash_entry.join(TRASH_META_FILE_NAME).exists());
    assert!(
        std::fs::canonicalize(&trash_entry)?.starts_with(std::fs::canonicalize(&s.root)?),
        "the trash entry must stay inside the fixture root"
    );

    let after = open_claims_db(Some(&claims_db))?;
    assert!(
        after
            .check_conflicts("other-mission", &work.to_string_lossy(), &isolation.claimed)?
            .is_empty(),
        "every claim must be released"
    );
    after.close()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// §6.1 cases 7 and 8
// ---------------------------------------------------------------------------

#[test]
fn cleanup_refuses_a_target_outside_the_fixture_root() -> TestResult {
    let s = Scratch::new("confine")?;
    let outside = Scratch::new("outside")?;
    let victim = outside.root.join("not-ours");
    std::fs::create_dir_all(&victim)?;

    let result = remove_worktree_confined(&victim, &s.root.join("trash"), &s.root);
    assert!(
        matches!(
            result,
            Err(GitError::OutsideConfinement {
                refusal: ConfinementRefusal::OutsideRoot,
                ..
            })
        ),
        "expected OutsideRoot, got {result:?}"
    );
    assert!(victim.exists(), "an out-of-root target must not be touched");
    Ok(())
}

#[test]
fn cleanup_refuses_a_symlinked_target() -> TestResult {
    let s = Scratch::new("symlink")?;
    let outside = Scratch::new("symlinked")?;
    let victim = outside.root.join("not-ours");
    std::fs::create_dir_all(&victim)?;

    let link = s.root.join("link");
    std::os::unix::fs::symlink(&victim, &link)?;

    let result = remove_worktree_confined(&link, &s.root.join("trash"), &s.root);
    assert!(
        matches!(
            result,
            Err(GitError::OutsideConfinement {
                refusal: ConfinementRefusal::Symlink,
                ..
            })
        ),
        "expected Symlink, got {result:?}"
    );
    assert!(victim.exists(), "the symlink target must not be touched");
    assert!(
        link.symlink_metadata()?.is_symlink(),
        "the link must remain"
    );

    // A link that points back *inside* the root is refused too:
    // renaming via a link moves something the caller did not name.
    let inside = s.root.join("inside");
    std::fs::create_dir_all(&inside)?;
    let inward = s.root.join("inward-link");
    std::os::unix::fs::symlink(&inside, &inward)?;
    assert!(matches!(
        remove_worktree_confined(&inward, &s.root.join("trash"), &s.root),
        Err(GitError::OutsideConfinement {
            refusal: ConfinementRefusal::Symlink,
            ..
        })
    ));
    assert!(inside.exists());
    Ok(())
}

// ---------------------------------------------------------------------------
// Push acknowledgement (§4.3) and the fixture PR adapter (§3)
// ---------------------------------------------------------------------------

#[test]
fn push_acknowledgement_needs_the_remote_to_show_the_pushed_sha() -> TestResult {
    let s = Scratch::new("pushack")?;
    let git = resolve_real_git()?;
    let origin = s.root.join("origin.git");
    seed_bare_remote(&git, &s.root, &origin, "base")?;
    let work = s.root.join("work");
    clone_repo(&git, &origin, &work)?;

    let base = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly)?;
    let branch = branch_name("mission-push", "push a branch");
    create_branch_from_refreshed_base(&work, &branch, &base)?;
    let worktree = work.join("wt");
    create_worktree(&work, &worktree, &branch)?;
    std::fs::write(worktree.join("pushed.txt"), "pushed\n")?;
    commit_all(&worktree, "phase: pushed work")?;
    let head = CommitSha::parse(&head_sha(&worktree)?).ok_or("head is not a full sha")?;

    // The branch is not on the remote yet: observably absent, not unobservable.
    assert_eq!(
        ls_remote_branch(&worktree, "origin", &branch, RemotePolicy::LocalOnly)?,
        None
    );

    let ack =
        push_with_acknowledgement(&worktree, "origin", &branch, &head, RemotePolicy::LocalOnly)?;
    assert_eq!(
        ack,
        PushAck::Acknowledged {
            remote_sha: head.clone()
        }
    );
    assert_eq!(
        ls_remote_branch(&worktree, "origin", &branch, RemotePolicy::LocalOnly)?,
        Some(head.clone())
    );

    // A push that exits zero while the remote shows a different sha is
    // ambiguous, never acknowledged: the exit code alone decides nothing.
    let ambiguous = push_with_acknowledgement(
        &worktree,
        "origin",
        &branch,
        base.base_sha(),
        RemotePolicy::LocalOnly,
    )?;
    assert_eq!(
        ambiguous,
        PushAck::Ambiguous,
        "a zero exit against a differing remote sha must not acknowledge"
    );

    // A genuine non-fast-forward refusal is a decided outcome.
    let publisher = s.root.join("publisher");
    clone_repo(&git, &origin, &publisher)?;
    commit_file(&git, &publisher, "theirs")?;
    run_git(&git, &publisher, &["push", "origin", "main"])?;
    commit_file(&git, &work, "ours")?;
    let rejected = {
        let ours = CommitSha::parse(&run_git(&git, &work, &["rev-parse", "HEAD"])?)
            .ok_or("head is not a full sha")?;
        push_with_acknowledgement(&work, "origin", "main", &ours, RemotePolicy::LocalOnly)?
    };
    assert_eq!(rejected, PushAck::Rejected);
    Ok(())
}

#[test]
fn fixture_pr_receipt_is_deterministic_and_offline() -> TestResult {
    let s = Scratch::new("fixturepr")?;
    let git = resolve_real_git()?;
    let origin = s.root.join("origin.git");
    seed_bare_remote(&git, &s.root, &origin, "base")?;
    let work = s.root.join("work");
    clone_repo(&git, &origin, &work)?;

    let base = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly)?;
    let branch = branch_name("mission-pr", "open a pull request");
    create_branch_from_refreshed_base(&work, &branch, &base)?;
    let worktree = work.join("wt");
    create_worktree(&work, &worktree, &branch)?;
    std::fs::write(worktree.join("pr.txt"), "pr\n")?;
    commit_all(&worktree, "phase: work for a pr")?;
    let head = CommitSha::parse(&head_sha(&worktree)?).ok_or("head is not a full sha")?;

    let ledger = s.root.join("pr-ledger");
    let adapter = FixturePrAdapter::new(origin.clone(), ledger.clone());
    let request = PrRequest {
        repo_root: work.clone(),
        base_branch: base.base_branch().to_owned(),
        head_branch: branch.clone(),
        head_sha: head.clone(),
        title: "phase work".to_owned(),
        body: "body".to_owned(),
        draft: false,
    };

    // A pull request may not exist for a commit the remote has not accepted.
    assert!(matches!(
        adapter.open(&request),
        Err(PrAdapterError::HeadNotPublished)
    ));

    assert_eq!(
        push_with_acknowledgement(&worktree, "origin", &branch, &head, RemotePolicy::LocalOnly)?,
        PushAck::Acknowledged {
            remote_sha: head.clone()
        }
    );

    let first = adapter.open(&request)?;
    let second = adapter.open(&request)?;
    assert_eq!(
        first, second,
        "re-opening the same head must not mint a second pull request"
    );
    assert!(first.url.starts_with("fixture://"), "url: {}", first.url);
    assert!(!first.url.contains("github"), "url: {}", first.url);
    assert_eq!(first.head_sha, head.as_str());
    assert_eq!(adapter.lookup(&first.receipt_id)?, Some(first.clone()));
    assert_eq!(adapter.lookup("fixture-pr-0000000000000000")?, None);

    let ledger_files: Vec<_> = std::fs::read_dir(&ledger)?.collect::<Result<_, _>>()?;
    assert_eq!(
        ledger_files.len(),
        1,
        "exactly one receipt file is expected"
    );

    // A different head is a different pull request.
    std::fs::write(worktree.join("pr2.txt"), "pr2\n")?;
    commit_all(&worktree, "phase: more work")?;
    let head2 = CommitSha::parse(&head_sha(&worktree)?).ok_or("head is not a full sha")?;
    assert_eq!(
        push_with_acknowledgement(
            &worktree,
            "origin",
            &branch,
            &head2,
            RemotePolicy::LocalOnly
        )?,
        PushAck::Acknowledged {
            remote_sha: head2.clone()
        }
    );
    let other = adapter.open(&PrRequest {
        head_sha: head2,
        ..request
    })?;
    assert_ne!(other.receipt_id, first.receipt_id);
    Ok(())
}

// ---------------------------------------------------------------------------
// §6.1 case 9 — the aggregated offline proof
// ---------------------------------------------------------------------------

const CHILD_ROOT_ENV: &str = "ORCHESTRATOR_GIT_E2E_CHILD_ROOT";
const CHILD_GIT_ENV: &str = "ORCHESTRATOR_GIT_E2E_REAL_GIT";
const CHILD_CASE: &str = "no_network_and_no_pr_provider_is_contacted";

fn write_shim(path: &Path, script: &str) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Splits one recorded line into its tab-separated argv fields.
fn argv_fields(line: &str) -> Vec<&str> {
    line.split('\t').filter(|field| !field.is_empty()).collect()
}

/// Reports whether a field names a remote git could reach over a network:
/// any `scheme://` other than `file://`, or the scp-like `[user@]host:path`.
fn field_is_a_network_url(field: &str) -> bool {
    if field.starts_with("file://") {
        return false;
    }
    if field.contains("://") {
        return true;
    }
    let head = field.split('/').next().unwrap_or(field);
    head.contains(':') && head.contains('@')
}

/// The workload the recording child runs. Every git call below is issued by
/// the library itself, so the shim records all of them.
fn offline_workload(root: &Path, git: &Path) -> TestResult {
    std::fs::create_dir_all(root)?;
    let origin = root.join("origin.git");
    seed_bare_remote(git, root, &origin, "base")?;
    let work = root.join("work");
    clone_repo(git, &origin, &work)?;
    run_git(
        git,
        &work,
        &["remote", "add", "upstream", "https://example.invalid/x.git"],
    )?;

    // A non-local remote is refused without a fetch.
    assert!(matches!(
        resolve_refreshed_base(&work, "upstream", "main", RemotePolicy::LocalOnly),
        Err(GitError::RemoteNotLocal { .. })
    ));

    let base = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly)?;
    let branch = branch_name("mission-offline", "offline aggregate");
    create_branch_from_refreshed_base(&work, &branch, &base)?;
    let worktree = work.join("wt");
    create_worktree(&work, &worktree, &branch)?;
    write_lock(&worktree, "mission-offline")?;
    std::fs::write(worktree.join("offline.txt"), "offline\n")?;
    commit_all(&worktree, "phase: offline aggregate")?;
    let head = CommitSha::parse(&head_sha(&worktree)?).ok_or("head is not a full sha")?;
    assert_eq!(
        push_with_acknowledgement(&worktree, "origin", &branch, &head, RemotePolicy::LocalOnly)?,
        PushAck::Acknowledged {
            remote_sha: head.clone()
        }
    );

    let adapter = FixturePrAdapter::new(origin.clone(), root.join("pr-ledger"));
    let receipt = adapter.open(&PrRequest {
        repo_root: work.clone(),
        base_branch: base.base_branch().to_owned(),
        head_branch: branch.clone(),
        head_sha: head,
        title: "offline".to_owned(),
        body: "offline".to_owned(),
        draft: false,
    })?;
    std::fs::write(root.join("pr-url.txt"), &receipt.url)?;

    remove_lock(&worktree);
    remove_worktree_confined(&worktree, &root.join("trash"), root)?;
    Ok(())
}

#[test]
fn no_network_and_no_pr_provider_is_contacted() -> TestResult {
    if let (Ok(root), Ok(git)) = (std::env::var(CHILD_ROOT_ENV), std::env::var(CHILD_GIT_ENV)) {
        return offline_workload(Path::new(&root), Path::new(&git));
    }

    let s = Scratch::new("offline")?;
    let real_git = resolve_real_git()?;
    let shim = s.root.join("shim");
    std::fs::create_dir_all(&shim)?;
    let record = s.root.join("git-argv.log");
    let gh_marker = s.root.join("gh-was-spawned");

    write_shim(
        &shim.join("git"),
        &format!(
            "#!/bin/sh\nprintf '%s\\t' \"$@\" >> '{record}'\nprintf '\\n' >> '{record}'\nexec '{real_git}' \"$@\"\n",
            record = record.display(),
            real_git = real_git.display(),
        ),
    )?;
    // If anything ever reaches for the PR provider, this shim records it and
    // fails the call rather than letting it succeed silently.
    write_shim(
        &shim.join("gh"),
        &format!(
            "#!/bin/sh\n: > '{marker}'\nexit 1\n",
            marker = gh_marker.display()
        ),
    )?;

    let child_root = s.root.join("child");
    let path = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(std::env::current_exe()?)
        .args([CHILD_CASE, "--exact", "--nocapture", "--test-threads=1"])
        .env("PATH", path)
        .env(CHILD_ROOT_ENV, &child_root)
        .env(CHILD_GIT_ENV, &real_git)
        .output()?;
    assert!(
        output.status.success(),
        "recorded child workload failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded = std::fs::read_to_string(&record)?;
    let lines: Vec<Vec<&str>> = recorded
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(argv_fields)
        .collect();
    assert!(
        !lines.is_empty(),
        "the recorder captured nothing; the shim was not on the child's PATH"
    );

    // Anti-vacuity: every negative assertion below is scoped to a command
    // shape, so each of those shapes must actually appear in the recording.
    // Without this, a shim that captured nothing would pass silently.
    for expected in ["fetch", "branch", "worktree", "push", "ls-remote"] {
        assert!(
            lines.iter().any(|fields| fields.first() == Some(&expected)),
            "no `git {expected}` was recorded, so the assertions below prove nothing"
        );
    }

    for fields in &lines {
        for field in fields {
            assert!(
                !field_is_a_network_url(field),
                "a network remote reached git argv: {field:?} in {fields:?}"
            );
        }
        assert_ne!(
            fields.first(),
            Some(&"gh"),
            "the git shim recorded a gh invocation: {fields:?}"
        );
        // Every branch this path creates is cut from a 40-hex sha, never a name.
        if fields.first() == Some(&"branch") {
            let base = fields.get(2).copied().unwrap_or_default();
            assert!(
                CommitSha::parse(base).is_some(),
                "git branch must take a 40-hex base, got {base:?}"
            );
        }
        // The refused non-local remote never became a fetch.
        if fields.first() == Some(&"fetch") {
            assert!(
                !fields.contains(&"upstream"),
                "the non-local remote was fetched: {fields:?}"
            );
        }
    }

    assert!(
        !gh_marker.exists(),
        "the gh shim marker exists, so a PR provider was contacted"
    );
    let pr_url = std::fs::read_to_string(child_root.join("pr-url.txt"))?;
    assert!(
        pr_url.starts_with("fixture://"),
        "the pull-request url must be non-resolvable, got {pr_url}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// CF-M3-W1 — a remote name is classified before the connection, not after
// ---------------------------------------------------------------------------

const PUSH_CHILD_ROOT_ENV: &str = "ORCHESTRATOR_GIT_E2E_PUSH_CHILD_ROOT";
const PUSH_CHILD_GIT_ENV: &str = "ORCHESTRATOR_GIT_E2E_PUSH_REAL_GIT";
const PUSH_CHILD_CASE: &str = "push_and_ls_remote_refuse_a_non_file_remote_before_any_network_call";

/// Pushes and lists against a `https://` remote and then against the local
/// one, so the recording holds both the refusals and a real push to compare
/// them with.
fn non_local_push_workload(root: &Path, git: &Path) -> TestResult {
    std::fs::create_dir_all(root)?;
    let origin = root.join("origin.git");
    seed_bare_remote(git, root, &origin, "base")?;
    let work = root.join("work");
    clone_repo(git, &origin, &work)?;
    run_git(
        git,
        &work,
        &["remote", "add", "upstream", "https://example.invalid/x.git"],
    )?;
    let head = CommitSha::parse(&head_sha(&work)?).ok_or("head is not a full sha")?;

    let pushed =
        push_with_acknowledgement(&work, "upstream", "main", &head, RemotePolicy::LocalOnly);
    assert!(
        matches!(pushed, Err(GitError::RemoteNotLocal { .. })),
        "expected RemoteNotLocal from push, got {pushed:?}"
    );
    let listed = ls_remote_branch(&work, "upstream", "main", RemotePolicy::LocalOnly);
    assert!(
        matches!(listed, Err(GitError::RemoteNotLocal { .. })),
        "expected RemoteNotLocal from ls-remote, got {listed:?}"
    );

    // The same two calls against the local remote must still work, so the
    // recording contains a genuine `push` and `ls-remote` for the negative
    // assertions to be scoped against.
    let base = resolve_refreshed_base(&work, "origin", "main", RemotePolicy::LocalOnly)?;
    let branch = branch_name("mission-nonlocal", "refused push");
    create_branch_from_refreshed_base(&work, &branch, &base)?;
    let worktree = work.join("wt");
    create_worktree(&work, &worktree, &branch)?;
    std::fs::write(worktree.join("local.txt"), "local\n")?;
    commit_all(&worktree, "phase: local push")?;
    let local_head = CommitSha::parse(&head_sha(&worktree)?).ok_or("head is not a full sha")?;
    assert_eq!(
        push_with_acknowledgement(
            &worktree,
            "origin",
            &branch,
            &local_head,
            RemotePolicy::LocalOnly
        )?,
        PushAck::Acknowledged {
            remote_sha: local_head
        }
    );
    Ok(())
}

/// `push_with_acknowledgement` and `ls_remote_branch` used to take a remote
/// *name* and let git resolve its URL at connect time, which put the only
/// classification of that URL after the connection. Both now classify first.
///
/// The refusal is observable rather than asserted: the workload runs under a
/// `PATH` whose `git` records every argv, and the recording must contain the
/// `remote get-url upstream` that did the classification, a real `push` and a
/// real `ls-remote` for the local remote, and no `push` or `ls-remote` naming
/// `upstream` or any network URL at all.
#[test]
fn push_and_ls_remote_refuse_a_non_file_remote_before_any_network_call() -> TestResult {
    if let (Ok(root), Ok(git)) = (
        std::env::var(PUSH_CHILD_ROOT_ENV),
        std::env::var(PUSH_CHILD_GIT_ENV),
    ) {
        return non_local_push_workload(Path::new(&root), Path::new(&git));
    }

    let s = Scratch::new("nonlocalpush")?;
    let real_git = resolve_real_git()?;
    let shim = s.root.join("shim");
    std::fs::create_dir_all(&shim)?;
    let record = s.root.join("git-argv.log");
    write_shim(
        &shim.join("git"),
        &format!(
            "#!/bin/sh\nprintf '%s\\t' \"$@\" >> '{record}'\nprintf '\\n' >> '{record}'\nexec '{real_git}' \"$@\"\n",
            record = record.display(),
            real_git = real_git.display(),
        ),
    )?;

    let child_root = s.root.join("child");
    let path = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(std::env::current_exe()?)
        .args([
            PUSH_CHILD_CASE,
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PATH", path)
        .env(PUSH_CHILD_ROOT_ENV, &child_root)
        .env(PUSH_CHILD_GIT_ENV, &real_git)
        .output()?;
    assert!(
        output.status.success(),
        "recorded child workload failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded = std::fs::read_to_string(&record)?;
    let lines: Vec<Vec<&str>> = recorded
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(argv_fields)
        .collect();

    // Anti-vacuity: the classification ran, and both refused commands really
    // do appear in this recording for the local remote.
    assert!(
        lines
            .iter()
            .any(|fields| fields == &["remote", "get-url", "upstream"]),
        "no `git remote get-url upstream` was recorded, so nothing classified the remote"
    );
    for expected in ["push", "ls-remote"] {
        assert!(
            lines.iter().any(|fields| fields.first() == Some(&expected)),
            "no `git {expected}` was recorded, so the assertions below prove nothing"
        );
    }

    for fields in &lines {
        if matches!(fields.first(), Some(&"push") | Some(&"ls-remote")) {
            assert!(
                !fields.contains(&"upstream"),
                "a refused remote reached the wire: {fields:?}"
            );
        }
        for field in fields {
            assert!(
                !field_is_a_network_url(field),
                "a network remote reached git argv: {field:?} in {fields:?}"
            );
        }
    }
    Ok(())
}
