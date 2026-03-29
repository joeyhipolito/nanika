# PR Workflow: Branch Isolation & Code Review Integration

> Design document for adding git branch management, PR creation, and review
> integration to the orchestrator. Based on external tool research (Devin,
> SWE-agent, OpenHands, Codex) and an audit of the orchestrator's current
> architecture.

---

## 1. Current State Analysis

### What exists today

The orchestrator has **no git integration beyond target resolution**. The only
git-aware code is `findGitRoot()` in `internal/cmd/run.go:1325`, which walks up
from a directory looking for `.git` to determine the target repository for
routing context.

| Capability | Status |
|---|---|
| Git root detection | Implemented (`findGitRoot`) |
| Target context from repo | Implemented (`buildTargetContext`) |
| Branch creation | None |
| Commit management | None |
| PR creation | None |
| Worktree isolation | None |
| Cross-mission conflict detection | None |
| Post-execution git operations | None |

### How isolation works today

Each mission gets a **filesystem workspace** under `~/.alluka/workspaces/<id>/`
with the structure:

```
~/.alluka/workspaces/<id>/
├── mission.md            # original task
├── checkpoint.json       # plan + phase states
├── events.ndjson         # event log
├── workers/<phase>/      # per-phase worker dirs
│   ├── CLAUDE.md         # persona + context
│   └── output.md         # worker output
├── artifacts/            # phase artifacts
└── learnings/            # extracted learnings
```

Workers execute in the **target repository directory** (`Workspace.TargetDir`,
resolved from git root) while writing state to their `WorkerDir`. This means
all phases of a mission — and all concurrent missions targeting the same repo —
share the same working tree. There is no branch or worktree isolation.

### Consequences of the current design

1. **Dirty-tree interference**: Two parallel missions editing the same file
   will silently clobber each other.
2. **No rollback**: A failed mission leaves partial changes in the working
   tree with no clean way to revert.
3. **Manual PR workflow**: Users must manually create branches, commit, and
   open PRs after a mission completes.
4. **No review integration**: The orchestrator's built-in review loop
   (`engine/review_loop.go`) operates on worker output text, not on actual
   diffs. External code review tools (Codex, GitHub reviewers) have no
   structured integration point.

---

## 2. External Tool Findings

Research into how other agent systems handle branch isolation and PR creation
(full details in memory: `project_branch_pr_research.md`).

### Summary matrix

| Tool | Isolation | Branch naming | PR creation | Parallel strategy |
|---|---|---|---|---|
| **Devin** | VM per session | `devin/<desc>` | Auto at end | MultiDevin: manager+workers, merge successful |
| **SWE-agent** | Container per run | N/A (patch output) | Opt-in CLI flag | No built-in coordinator |
| **OpenHands** | Container per session | `openhands-fix-issue-<N>` | Draft on success, bare push on fail | Hierarchical integration branch |
| **Codex Cloud** | Container per task | Auto-named | Draft PR support | Independent sandboxes (~5 concurrent) |
| **Codex local** | Git worktrees (detached HEAD) | N/A | N/A | No conflict detection |

### Key takeaways

1. **No tool uses file-level locking.** Conflict avoidance is always via task
   decomposition scoping — the orchestrator's DAG dependency system already
   provides this for intra-mission phases.

2. **Git worktrees are the lightest-weight isolation primitive** that doesn't
   require containers or VMs. Codex local already validates this approach.

3. **Branch naming conventions are universal** — every tool that creates
   branches uses a `<tool>/<descriptor>` pattern for namespace isolation.

4. **Draft PRs are the safe default.** Both OpenHands and Codex Cloud create
   drafts rather than ready-for-review PRs.

5. **Review integration is underdeveloped everywhere.** No tool has deep
   review-loop integration with external code review tools — this is a
   differentiation opportunity.

---

## 3. Proposed Architecture

### 3.1 Branch naming convention

```
via/<mission-id>/<slug>
```

- `via/` prefix namespaces all orchestrator branches.
- `<mission-id>` is the workspace ID (e.g., `20260316-ab12cd34`).
- `<slug>` is a sanitized, truncated form of the mission task (max 40 chars,
  lowercase, hyphens for spaces, strip special chars).

Examples:
```
via/20260316-ab12cd34/implement-login-feature
via/20260316-ef56gh78/fix-race-condition-in-worker
```

**Rationale**: Mission-scoped branches (not phase-scoped) because phases
operate on the same logical unit of work. The mission boundary is the natural
commit/PR boundary.

### 3.2 Git worktree per mission

Each mission that targets a git repository gets an isolated worktree:

```
~/.alluka/worktrees/<mission-id>/
```

**Lifecycle:**

```
Mission Start
  │
  ├─ Resolve target repo (existing findGitRoot)
  ├─ Create branch: via/<mission-id>/<slug> from current HEAD
  ├─ Create worktree: git worktree add ~/.alluka/worktrees/<id> <branch>
  ├─ Set Workspace.TargetDir = worktree path
  │
  ├─ ... phases execute in worktree ...
  │
  ├─ Mission completes
  │   ├─ Stage + commit changes in worktree
  │   ├─ Push branch (if remote configured)
  │   ├─ Optionally create draft PR
  │   └─ Clean up worktree: git worktree remove
  │
  └─ Mission fails
      ├─ Leave worktree for inspection (user can resume)
      └─ Clean up on explicit `orchestrator cleanup`
```

**Why worktrees over containers:**
- Zero infrastructure overhead (no Docker required).
- Git enforces branch exclusivity — two worktrees can never check out the same
  branch, preventing accidental cross-mission interference.
- Workers already execute via `sdk.QueryText()` which spawns Claude Code CLI
  as a subprocess — the subprocess inherits the worktree CWD naturally.
- Worktrees share the object store with the main repo, so cloning cost is
  near-zero.

**New fields on `Workspace`:**

```go
type Workspace struct {
    // ... existing fields ...

    // Git isolation (populated when target is a git repo)
    GitRepoRoot  string // original repo root (from findGitRoot)
    WorktreePath string // ~/.alluka/worktrees/<id>/ (empty if no git target)
    BranchName   string // via/<id>/<slug>
    BaseBranch   string // branch worktree was created from (e.g., "main")
}
```

### 3.3 Commit strategy

**Auto-commit on mission completion** with a structured commit message:

```
via(<mission-id>): <task summary>

Mission: <full task text, first 200 chars>
Phases: <phase-1>, <phase-2>, ...
Personas: <persona-1>, <persona-2>, ...

Generated by orchestrator v<version>
```

**Rules:**
- Only commit when all phases complete successfully (no partial commits on
  failure — the worktree preserves the state for resume).
- Commit includes all tracked file changes in the worktree.
- Untracked files are staged only if they were created by worker tool-use
  events (detectable from event log).
- Never commit `.env`, credentials, or files matching `.gitignore`.

### 3.4 PR creation

**Opt-in via mission frontmatter** or CLI flag:

```markdown
# Mission: Implement login feature
publish_pr: true
pr_reviewers: @alice, @bob
---
```

```bash
orchestrator run task.md --pr
orchestrator run "fix the bug" --pr --draft
```

**Implementation:**

```go
type PRConfig struct {
    Enabled   bool     // from frontmatter publish_pr or --pr flag
    Draft     bool     // default true; --no-draft to override
    Reviewers []string // from frontmatter pr_reviewers
    Labels    []string // from frontmatter pr_labels
    Base      string   // target branch (default: BaseBranch from worktree)
}
```

PR creation uses `gh pr create` (GitHub CLI) — avoids importing GitHub API
client libraries and leverages the user's existing auth.

**PR body template:**

```markdown
## Summary

<LLM-generated summary from mission task + phase outputs>

## Changes

<file change summary extracted from event log tool-use events>

## Mission Details

- **Mission ID:** <workspace-id>
- **Phases:** <count> (<parallel|sequential>)
- **Personas:** <comma-separated>
- **Cost:** $<total> (<tokens-in> in / <tokens-out> out)
- **Duration:** <elapsed>

## Review Notes

<if review loop ran: blockers found + resolved, warnings>
```

### 3.5 Review integration with Codex via @codex mention

The orchestrator's internal review loop (`engine/review_loop.go`) already
performs structural code review. For external review integration, we add a
**post-PR review hook** that can invoke Codex or other review tools.

**Flow:**

```
Mission completes → PR created (draft)
  │
  ├─ If codex_review: true in frontmatter
  │   ├─ Add comment to PR: "@codex please review this PR"
  │   ├─ Emit event: review.external_requested
  │   └─ (Codex picks up via GitHub webhook, posts review comments)
  │
  ├─ If review_loop ran during mission
  │   ├─ Post review findings as PR comment
  │   └─ Include blockers resolved + warnings outstanding
  │
  └─ Mark PR ready-for-review (if --no-draft)
```

**@codex integration specifics:**

Codex Cloud monitors GitHub for `@codex` mentions in PR comments. By posting a
structured mention, the orchestrator can trigger an automated code review pass
after the PR is created. This creates a two-layer review:

1. **Internal review** (during mission): The orchestrator's own
   `staff-code-reviewer` persona catches structural issues, missing tests,
   and constraint violations before the code leaves the mission.
2. **External review** (post-PR): Codex reviews the full diff in context,
   catching integration issues, API misuse, and broader architectural concerns
   that single-phase review can miss.

**Configuration:**

```go
type ReviewConfig struct {
    // Internal review (existing)
    MaxReviewLoops int  // default 1, max 3

    // External review (new)
    CodexReview    bool   // trigger @codex review on PR
    CodexPrompt    string // custom review instructions (optional)
}
```

### 3.6 Cross-mission conflict detection

For parallel missions targeting the same repository, we add a **file-claim
registry** in SQLite (stored in `learnings.db` alongside routing data):

```sql
CREATE TABLE file_claims (
    file_path   TEXT NOT NULL,
    mission_id  TEXT NOT NULL,
    claimed_at  DATETIME NOT NULL,
    released_at DATETIME,           -- NULL = still claimed
    PRIMARY KEY (file_path, mission_id)
);
```

**Semantics:**
- Before a mission starts, check if any files in the target directory are
  claimed by another active mission.
- Claims are advisory, not blocking — warn the user, don't prevent execution.
- Claims are released when the mission completes or is cancelled.
- The decomposition prompt can include claimed files as constraints so the LLM
  avoids editing them.

**Why advisory, not blocking:** Following the universal pattern from external
tools — no agent system uses file-level locking. Task decomposition scoping
(the orchestrator's DAG dependencies) is the primary conflict avoidance
mechanism. The claim registry adds visibility, not enforcement.

---

## 4. MVP Scope vs Follow-on Work

### MVP (v1)

Minimum viable git integration — branch isolation and basic PR creation.

| Feature | Details |
|---|---|
| **Worktree creation** | `git worktree add` on mission start when target is a git repo |
| **Branch naming** | `via/<mission-id>/<slug>` convention |
| **Auto-commit** | Commit all changes on mission success |
| **Worktree cleanup** | Remove worktree on success; preserve on failure |
| **`--pr` flag** | Create draft PR via `gh pr create` |
| **PR body** | Structured template with mission metadata |
| **`--no-git` flag** | Opt out of git isolation (legacy behavior) |
| **Workspace fields** | Add `GitRepoRoot`, `WorktreePath`, `BranchName`, `BaseBranch` |

**Not in MVP:**
- No Codex review integration.
- No file-claim registry.
- No LLM-generated PR summaries (use task text directly).
- No custom PR reviewers/labels from frontmatter.
- No auto-push (require explicit `--push` or `--pr` to push).

### Follow-on: Review integration (v2)

| Feature | Details |
|---|---|
| **Codex @mention** | Post `@codex review` comment on PR creation |
| **Review findings as PR comments** | Post internal review loop results on the PR |
| **Frontmatter PR config** | `publish_pr`, `pr_reviewers`, `pr_labels`, `codex_review` |
| **LLM PR summary** | Generate summary from phase outputs |
| **PR status tracking** | Store PR URL/number in workspace sidecar |

### Follow-on: Parallel safety (v3)

| Feature | Details |
|---|---|
| **File-claim registry** | SQLite table for advisory cross-mission claims |
| **Claim injection into decomposition** | Warn LLM about files claimed by other missions |
| **Conflict detection on commit** | Check if base branch moved; rebase or warn |
| **Integration branch** | OpenHands-style hierarchical merge for related missions |

### Follow-on: Advanced git operations (v4)

| Feature | Details |
|---|---|
| **Auto-rebase** | Rebase worktree branch on base before PR |
| **Stacked PRs** | Multi-phase missions creating chained PRs |
| **PR merge on audit pass** | Auto-merge when audit score meets threshold |
| **Branch protection awareness** | Check branch protection rules before push |

---

## 5. Migration Plan for Existing Missions

### Backward compatibility

The git integration is **strictly additive**. Existing behavior is preserved:

1. **No git target** → No worktree created. Phases execute in `WorkerDir`
   (legacy behavior, unchanged).
2. **Git target, `--no-git`** → Phases execute in target repo working tree
   directly (current behavior, unchanged).
3. **Git target, default** → Worktree created. Phases execute in worktree.
   This is the only behavioral change, and it's transparent to workers.

### Rollout phases

**Phase A: Internal plumbing (no user-visible change)**

1. Add `GitRepoRoot`, `WorktreePath`, `BranchName`, `BaseBranch` fields to
   `Workspace` struct.
2. Add `gitBranchName(missionID, task string) string` helper for slug
   generation.
3. Add `createWorktree(repoRoot, branchName, worktreePath string) error` and
   `removeWorktree(worktreePath string) error` wrappers around `git worktree`
   commands.
4. Add `commitWorktree(worktreePath, message string) error` wrapper.
5. Unit tests for all helpers.

**Phase B: Opt-in worktree isolation**

1. Add `--git-isolate` flag to `orchestrator run` (default false initially).
2. When enabled: create worktree, set `Workspace.TargetDir` to worktree path,
   auto-commit on success, clean up worktree.
3. Persist git metadata in `checkpoint.json` so `--resume` can re-attach to
   the worktree.
4. Add `orchestrator cleanup --worktrees` to clean up orphaned worktrees.
5. Integration tests with a test repository.

**Phase C: Default-on + PR creation**

1. Flip `--git-isolate` default to `true` for git-targeted missions.
2. Add `--no-git` flag for users who want the old behavior.
3. Add `--pr` flag for draft PR creation via `gh`.
4. Add PR body template rendering.
5. Emit new events: `git.worktree_created`, `git.committed`,
   `git.pr_created`.

**Phase D: Review integration**

1. Add `codex_review` frontmatter field + `--codex-review` flag.
2. Post `@codex` mention as PR comment.
3. Post internal review findings as PR comments.
4. Add `pr_reviewers`, `pr_labels` frontmatter support.

### Data migration

No database migration required. The new `Workspace` fields are persisted in
`checkpoint.json` (JSON, not SQL) and are backward-compatible — old
checkpoints simply have empty/zero values for the git fields. The `--resume`
path checks for `WorktreePath != ""` to determine whether to re-attach to a
worktree or fall back to legacy execution.

The only new SQL table (`file_claims`) is created lazily on first use (v3),
following the same pattern as `routing_patterns` and `phase_shape_patterns`.

---

## 6. Implementation Notes

### Git operations wrapper

All git operations go through a new `internal/git/` package (not shell-outs
scattered across cmd code):

```go
package git

func FindRoot(dir string) string                          // move from cmd/run.go
func CreateBranch(repoRoot, name, base string) error
func CreateWorktree(repoRoot, path, branch string) error
func RemoveWorktree(path string) error
func CommitAll(worktreePath, message string) error
func Push(worktreePath, remote, branch string) error
func CurrentBranch(repoRoot string) (string, error)
func HasUncommittedChanges(path string) (bool, error)
```

### gh CLI dependency

PR creation depends on `gh` being installed and authenticated. The
orchestrator should:

1. Check for `gh` in PATH before attempting PR creation.
2. Fail gracefully with a clear message if `gh` is missing.
3. Never store or handle GitHub tokens directly — delegate entirely to `gh`.

### Event system additions

New event types for git operations:

```go
const (
    EventGitWorktreeCreated EventType = "git.worktree_created"
    EventGitCommitted       EventType = "git.committed"
    EventGitPushed          EventType = "git.pushed"
    EventGitPRCreated       EventType = "git.pr_created"
    EventReviewExternal     EventType = "review.external_requested"
)
```

### Checkpoint schema evolution

`checkpoint.json` gains git-related fields via the `Workspace` struct. Old
checkpoints without these fields deserialize cleanly (Go zero values). No
versioning or migration logic needed.

### Testing strategy

- **Unit tests**: Branch name generation, commit message formatting, worktree
  lifecycle helpers (using `git init` in `t.TempDir()`).
- **Integration tests**: Full mission run with git isolation in a test repo,
  verifying branch creation, commit content, and worktree cleanup.
- **PR creation tests**: Mock `gh` binary (shell script that records args) to
  verify correct `gh pr create` invocation without hitting GitHub.

---

## 7. Open Questions

1. **Worktree path**: Should worktrees live under `~/.alluka/worktrees/` (parallel
   to `workspaces/`) or inside the workspace directory itself? The former is
   cleaner for cleanup; the latter keeps everything co-located.

2. **Multi-repo missions**: A mission that targets multiple repositories (e.g.,
   "update the API server and the client SDK") would need multiple worktrees.
   Defer to v4 or handle via phase-level `TargetDir` overrides?

3. **Rebase vs merge**: When the base branch has moved during mission
   execution, should the orchestrator auto-rebase before PR creation, or
   leave that to the user? Auto-rebase risks conflicts; manual rebase
   preserves user control.

4. **Commit granularity**: One commit per mission (proposed) vs one commit per
   phase? Per-mission is simpler and matches the PR boundary. Per-phase gives
   better `git bisect` granularity but creates noisy history.

5. **Force push policy**: If a mission is resumed and re-executed, should the
   branch be force-pushed? This is the only way to update an existing PR
   branch, but risks overwriting manual changes the user made to the branch.
