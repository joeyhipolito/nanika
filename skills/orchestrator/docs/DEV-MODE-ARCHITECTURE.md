# Dev-Mode Architecture: Making Dev Capabilities Optional

**Status:** Proposal
**Date:** 2026-03-17
**Context:** Code audit of `internal/cmd/run.go` (1784 LOC), `internal/engine/`, `internal/core/runtime.go`, `internal/git/`, `internal/claims/`, `internal/routing/`; external survey of 8 AI dev tools (Cursor, Windsurf, Devin, SWE-agent, OpenHands, Copilot, Aider, Claude Code)

---

## Problem Statement

The orchestrator's core execution pipeline — DAG-based phase decomposition, persona routing, parallel execution, checkpointing — is domain-agnostic. But dev-specific capabilities (git isolation, PR creation, Codex review, file claims, review loops, routing DB) are hardwired throughout `cmd/run.go` and `engine/`. This coupling means:

1. **Non-dev domains pay the cost.** Git checks, claims DB opens, and routing queries execute even when the mission is creative writing or research.
2. **New dev capabilities require modifying the core pipeline.** Adding CI integration, deployment hooks, or language-server features means editing `runTask()` directly.
3. **Testing is tangled.** Dev subsystem failures can break domain-agnostic execution paths.

The `--domain` flag exists but only filters learnings — it doesn't gate which subsystems initialize.

## Current Integration Points

Dev-specific code touches the pipeline in six places:

| # | Where | What | Lines |
|---|-------|------|-------|
| 1 | **Flag parsing** | `--git-isolate`, `--no-git`, `--pr`, `--no-draft`, `--codex-review` | `run.go:36-41, 77-85` |
| 2 | **Workspace mutation** | `GitRepoRoot`, `WorktreePath`, `BranchName`, `BaseBranch` fields | `run.go:239-243` |
| 3 | **Pre-execution** | `setupGitIsolation()`, `claims.OpenDB()`, `ClaimFiles()`, `CheckConflicts()` | `run.go:239-281` |
| 4 | **Post-execution** | `teardownGitIsolation()`, `createMissionPR()`, codex review comment | `run.go:380-391` |
| 5 | **Routing context** | `buildTargetContext()` → routing DB queries for persona bias, shapes, corrections | `run.go:213-234` |
| 6 | **Event types** | `git.worktree_created`, `git.committed`, `git.pr_created`, `review.external_requested` | `event/types.go:62-69` |

Additionally, `engine/review_loop.go` injects fix phases after reviewer findings — a pattern that's dev-specific (code review → fix cycle) but structurally reusable (any feedback → response cycle).

## External Research Summary

Survey of 8 tools reveals two dominant patterns:

**Pattern A — MCP as domain boundary** (Cursor, Windsurf, Copilot, Claude Code): Dev tools are built-in; non-dev capabilities are opt-in via MCP server install. The de facto industry standard.

**Pattern B — Tool bundle composition** (SWE-agent): All capabilities are peer-level YAML bundles. Dev and non-dev are both opt-in configurations — no built-in vs. extension distinction.

**Lifecycle hooks** are rare. Only Claude Code has comprehensive pre/post hooks. SWE-agent has post-action `state_command`. Most tools have no interception points.

**Key principle from survey:** Name and register every capability. Anonymous hardcoded capabilities can't be toggled.

---

## Option Analysis

### Option A: Built-in Domain Config

Gate existing dev code behind the `--domain` flag. When `domain != "dev"`, skip git isolation, claims, routing, PR creation, and review loops.

```go
// In runTask(), guard each dev subsystem:
if domain == "dev" && gitIsolate && !noGit && ws.TargetDir != "" && git.FindRoot(ws.TargetDir) != "" {
    setupGitIsolation(ws, task, emitter, missionID)
}
```

**Pros:**
- Minimal code change (~30 LOC of `if` guards in `runTask()`)
- No new abstractions, no new interfaces
- Ships in a single PR
- Zero risk of breaking existing dev workflows

**Cons:**
- Doesn't solve extensibility — new dev capabilities still require `runTask()` edits
- `runTask()` accumulates more conditional branches (already 1784 LOC)
- No reuse story — a "deploy" domain can't opt into git isolation without being "dev"
- Binary domain membership: a capability is either "dev" or "not dev" forever
- Doesn't align with the external convergence toward named, composable capabilities

**Effort:** ~1 day. Low risk.

**Verdict:** Adequate as a stopgap. Does not address the structural problem.

### Option B: Plugin Interface (Lifecycle Hooks)

Define a `LifecyclePlugin` interface with named hooks at each pipeline stage. Dev capabilities become a `DevPlugin` that registers itself. New domains add new plugins without modifying the core.

```go
// Plugin interface — each method is optional (no-op default via embedding).
type LifecyclePlugin interface {
    Name() string

    // Pre-execution: called after workspace creation, before decomposition.
    OnWorkspaceCreated(ctx context.Context, ws *core.Workspace, task string, emitter event.Emitter) error

    // Post-decomposition: can modify the plan (inject review phases, etc.).
    OnPlanReady(ctx context.Context, ws *core.Workspace, plan *core.Plan) error

    // Post-execution: called after engine.Execute returns.
    OnMissionComplete(ctx context.Context, ws *core.Workspace, plan *core.Plan, result *core.ExecutionResult) error

    // Cleanup: always called, even on failure. Deferred.
    OnTeardown(ctx context.Context, ws *core.Workspace, success bool) error
}
```

The dev plugin would implement all four hooks:

| Hook | Dev behavior |
|------|-------------|
| `OnWorkspaceCreated` | `setupGitIsolation()`, `claims.ClaimFiles()`, `CheckConflicts()` |
| `OnPlanReady` | Inject review phases, apply routing context hints |
| `OnMissionComplete` | `teardownGitIsolation()`, `createMissionPR()`, codex review, shape recording |
| `OnTeardown` | `claims.ReleaseAll()`, worktree cleanup on failure |

```go
// Registration in runTask():
var plugins []LifecyclePlugin
if domain == "dev" || gitIsolate || createPR {
    plugins = append(plugins, NewDevPlugin(devPluginConfig{
        gitIsolate:  gitIsolate && !noGit,
        createPR:    createPR,
        noDraft:     noDraft,
        codexReview: codexReview,
    }))
}
```

**Pros:**
- Clean separation: `runTask()` becomes a domain-agnostic pipeline (~400 LOC saved)
- Extensible: deploy, CI, analytics plugins add without modifying core
- Testable: plugins can be unit-tested with mock workspaces
- Aligns with Claude Code's hook model (the strongest external precedent)
- Plugins are named — capability toggling becomes natural
- Review loop generalizes: any plugin can inject "feedback → fix" cycles via `OnPlanReady`

**Cons:**
- Moderate refactoring effort (~3-4 days)
- Interface design requires getting hook granularity right — too few hooks and plugins reach into internals; too many and the interface is unstable
- Plugin ordering and error semantics need specification
- Risk of over-abstraction if only one plugin ever exists

**Effort:** ~3-4 days. Medium risk (refactoring tested code paths).

**Verdict:** Recommended. The codebase is already approaching the complexity threshold where inline conditionals create maintenance burden. The plugin boundary also creates a natural test seam.

### Option C: Core/Dev Package Split

Physically separate `internal/cmd/run.go` into `internal/pipeline/` (domain-agnostic) and `internal/devmode/` (dev-specific). The pipeline package exposes injection points; devmode registers handlers at init time.

```
internal/
  pipeline/
    pipeline.go      # runTask equivalent, plugin-aware
    plugin.go        # LifecyclePlugin interface
    emitter.go       # event setup
  devmode/
    plugin.go        # DevPlugin implementation
    git.go           # setupGitIsolation, teardown, PR
    claims.go        # file claim integration
    routing.go       # buildTargetContext, shape recording
    review.go        # review loop injection
```

**Pros:**
- Strongest separation — import graph enforces no reverse dependencies
- `devmode` package can be excluded from builds (build tags) for minimal non-dev binaries
- Clear ownership boundaries for contributors

**Cons:**
- Highest effort (~5-7 days) — moves code across packages, rewires imports
- Splits `run.go` which is currently the single source of pipeline truth
- Risk of creating a "distributed monolith" where packages reach back through shared state
- Build tags are operationally complex for a CLI that's primarily dev-focused
- The plugin interface from Option B is a prerequisite anyway

**Effort:** ~5-7 days. Higher risk (import graph changes, potential circular deps).

**Verdict:** Premature. Worth revisiting when there are 3+ plugins or when binary size / build-tag isolation becomes a real requirement. Option B provides the interface that makes this migration mechanical later.

---

## Recommendation: Option B (Plugin Interface)

Option B provides the right abstraction at the right time. The codebase has exactly one domain's worth of capabilities to extract, and the plugin interface makes that extraction testable while leaving the door open for future domains.

Option A is too shallow — it addresses the symptom (non-dev domains running dev code) without the cause (capabilities hardwired into the pipeline). Option C is the right end state but premature — it requires Option B's interface anyway, and the physical split adds effort without proportional benefit today.

---

## Domain-to-Lifecycle Mapping

This table maps every current dev capability to its lifecycle hook, showing where each piece moves:

| Capability | Current location | Hook | Plugin method |
|-----------|-----------------|------|--------------|
| Git repo detection | `run.go:239` | Pre-execution | `OnWorkspaceCreated` |
| Branch creation | `git.CreateBranch` via `setupGitIsolation` | Pre-execution | `OnWorkspaceCreated` |
| Worktree creation | `git.CreateWorktree` via `setupGitIsolation` | Pre-execution | `OnWorkspaceCreated` |
| Workspace field mutation | `ws.GitRepoRoot`, `WorktreePath`, etc. | Pre-execution | `OnWorkspaceCreated` |
| File claims (acquire) | `claims.ClaimFiles` | Pre-execution | `OnWorkspaceCreated` |
| Conflict detection | `claims.CheckConflicts` | Pre-execution | `OnWorkspaceCreated` |
| Routing context resolution | `buildTargetContext` | Pre-execution | `OnWorkspaceCreated` (context enrichment) |
| Review phase injection | `engine/review_loop.go` | Post-decomposition | `OnPlanReady` |
| Codex runtime registration | `registerRuntimeExecutors` | Post-decomposition | `OnPlanReady` |
| Git workflow stripping | `stripGitWorkflowSection` | Pre-decomposition | `OnWorkspaceCreated` |
| Git commit (success) | `teardownGitIsolation` | Post-execution | `OnMissionComplete` |
| PR creation | `createMissionPR` | Post-execution | `OnMissionComplete` |
| Codex review comment | Inside `createMissionPR` | Post-execution | `OnMissionComplete` |
| Shape recording | `runPostExecutionRecorders` | Post-execution | `OnMissionComplete` |
| Mission file sync | `SyncManagedMissionStatus` | Post-execution | `OnMissionComplete` |
| File claims (release) | `claims.ReleaseAll` | Cleanup | `OnTeardown` |
| Worktree cleanup (failure) | `teardownGitIsolation` (preserve path) | Cleanup | `OnTeardown` |

### What stays in the core pipeline

These are domain-agnostic and remain in `runTask()`:

- Learning DB open/close
- Embedder setup
- Workspace creation
- Skill index loading
- Decomposition dispatch
- Engine creation and `Execute()`
- Checkpoint save/load
- Event emitter setup
- Template load/save
- Result printing

---

## Plugin Interface Specification

### Interface Definition

```go
package pipeline

import (
    "context"

    "github.com/joeyhipolito/orchestrator-cli/internal/core"
    "github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// LifecyclePlugin hooks into the mission execution pipeline.
// All methods receive a context that is cancelled on SIGINT/SIGTERM.
// Errors from any hook abort the mission (except OnTeardown, which logs and continues).
type LifecyclePlugin interface {
    // Name returns a unique identifier for this plugin (e.g. "dev", "deploy").
    Name() string

    // OnWorkspaceCreated is called after the workspace directory exists but
    // before decomposition. Plugins may mutate ws (e.g. set TargetDir,
    // WorktreePath) and enrich the pipeline context.
    //
    // task is the raw mission text (pre-stripping). Plugins that need to
    // modify task text (e.g. strip git workflow sections) return the modified
    // text; return "" to leave it unchanged.
    OnWorkspaceCreated(ctx context.Context, p *HookContext) (modifiedTask string, err error)

    // OnPlanReady is called after decomposition completes and before engine
    // execution. Plugins may modify the plan (inject phases, set runtimes,
    // reorder). The plan pointer is mutable.
    OnPlanReady(ctx context.Context, p *HookContext, plan *core.Plan) error

    // OnMissionComplete is called after engine.Execute returns, regardless
    // of success/failure. Plugins should check result.Success to decide
    // behavior (e.g. only create PRs on success).
    OnMissionComplete(ctx context.Context, p *HookContext, plan *core.Plan, result *core.ExecutionResult) error

    // OnTeardown is always called (deferred), even if earlier hooks failed.
    // Errors are logged but do not affect the mission exit code.
    // success reflects the final mission outcome.
    OnTeardown(ctx context.Context, p *HookContext, success bool) error
}

// HookContext carries shared state that plugins read and write.
// It replaces direct access to cmd-level variables and workspace fields.
type HookContext struct {
    Workspace  *core.Workspace
    Emitter    event.Emitter
    MissionID  string
    Task       string    // raw mission text
    Domain     string
    Verbose    bool

    // Plugins store opaque state here for cross-hook communication.
    // Key format: "pluginName.key" (e.g. "dev.claimsDB").
    PluginData map[string]any
}

// BasePlugin provides no-op defaults so plugins only override hooks they need.
type BasePlugin struct{}

func (BasePlugin) OnWorkspaceCreated(context.Context, *HookContext) (string, error) { return "", nil }
func (BasePlugin) OnPlanReady(context.Context, *HookContext, *core.Plan) error       { return nil }
func (BasePlugin) OnMissionComplete(context.Context, *HookContext, *core.Plan, *core.ExecutionResult) error {
    return nil
}
func (BasePlugin) OnTeardown(context.Context, *HookContext, bool) error { return nil }
```

### Plugin Execution Semantics

1. **Ordering:** Plugins execute in registration order. For v1, order is deterministic (dev plugin first). Future: explicit priority field if needed.
2. **Error propagation:** `OnWorkspaceCreated` and `OnPlanReady` errors abort the mission. `OnMissionComplete` errors are logged as warnings. `OnTeardown` errors are always logged, never fatal.
3. **Mutation:** `OnWorkspaceCreated` may mutate `Workspace` fields. `OnPlanReady` may mutate the `Plan`. Mutations are visible to subsequent plugins and the core pipeline.
4. **Cross-hook state:** Plugins use `HookContext.PluginData` to pass state between hooks (e.g. claims DB handle opened in `OnWorkspaceCreated`, closed in `OnTeardown`).

### DevPlugin Sketch

```go
package devmode

type DevPlugin struct {
    BasePlugin
    config DevPluginConfig
}

type DevPluginConfig struct {
    GitIsolate  bool
    CreatePR    bool
    NoDraft     bool
    CodexReview bool
    Reviewers   []string
    Labels      []string
}

func (d *DevPlugin) Name() string { return "dev" }

func (d *DevPlugin) OnWorkspaceCreated(ctx context.Context, p *HookContext) (string, error) {
    // 1. Detect git repo, create worktree + branch
    // 2. Open claims DB, claim files, check conflicts
    // 3. Build and store routing target context
    // 4. Strip git workflow section from task
    // 5. Store claimsDB in PluginData for teardown
    // Returns modified task text (git section stripped)
}

func (d *DevPlugin) OnPlanReady(ctx context.Context, p *HookContext, plan *core.Plan) error {
    // 1. Register Codex executor on the engine (via PluginData reference)
    // 2. Validate review phase injection criteria
    // (Review loop itself stays in engine — it's phase-execution-level, not lifecycle-level)
}

func (d *DevPlugin) OnMissionComplete(ctx context.Context, p *HookContext, plan *core.Plan, result *core.ExecutionResult) error {
    // 1. Commit + teardown worktree (success) or preserve (failure)
    // 2. Create PR if configured and successful
    // 3. Post codex review comment
    // 4. Record shapes to routing DB
    // 5. Sync mission file status
}

func (d *DevPlugin) OnTeardown(ctx context.Context, p *HookContext, success bool) error {
    // 1. Release file claims
    // 2. Close claims DB
    // 3. Cleanup worktree if still present on failure
}
```

---

## Migration Path

### Phase 1: Define interface, no behavior change (1 day)

1. Add `internal/pipeline/plugin.go` with `LifecyclePlugin`, `HookContext`, `BasePlugin`
2. Add `PluginRunner` that takes `[]LifecyclePlugin` and calls hooks in order
3. Add a `runPluginHook()` helper in `cmd/run.go` that wraps error handling and verbose logging
4. No functional changes — all tests pass, dev code stays inline

**Deliverable:** Interface compiles, `PluginRunner` tested with mock plugins.

### Phase 2: Extract DevPlugin (2-3 days)

1. Create `internal/devmode/plugin.go` implementing `LifecyclePlugin`
2. Move `setupGitIsolation()`, `teardownGitIsolation()`, `createMissionPR()` into `devmode/git.go`
3. Move claims integration into `devmode/claims.go`
4. Move `buildTargetContext()`, `runPostExecutionRecorders()` into `devmode/routing.go`
5. Move `stripGitWorkflowSection()` into `devmode/task.go`
6. In `cmd/run.go`:
   - Remove moved functions
   - Build `DevPlugin` from flags
   - Register it with `PluginRunner`
   - Replace inline calls with `runner.OnWorkspaceCreated()`, etc.
7. `cmd/run.go` shrinks from ~1784 to ~900 LOC

**Deliverable:** Identical behavior. `runTask()` is half its current size. All existing tests pass. Dev-specific code is behind an interface.

### Phase 3: Flag migration (0.5 day)

1. Move `--git-isolate`, `--no-git`, `--pr`, `--no-draft`, `--codex-review` from `cmd/run.go` into a `devmode.RegisterFlags(cmd)` function
2. Non-dev domains never see these flags in `--help`
3. DevPlugin is only instantiated when relevant flags are set or `--domain=dev`

**Deliverable:** Clean CLI surface per domain.

### Phase 4: Review loop generalization (1 day, optional)

1. The review loop in `engine/review_loop.go` currently hardcodes the "reviewer finds blockers → inject fix phase" pattern
2. Generalize to a `FeedbackLoop` that any plugin can configure via `OnPlanReady`:
   ```go
   type FeedbackLoop struct {
       TriggerRole   core.Role   // which role's output triggers the loop
       ResponseRole  core.Role   // what role the injected fix phase gets
       MaxIterations int
       ParseFindings func(output string) (blockers, warnings []string)
   }
   ```
3. DevPlugin registers the code-review feedback loop; a future "editorial review" plugin could register a different one

**Deliverable:** Review loops are plugin-configurable. No engine changes needed for new feedback patterns.

---

## What This Enables

With the plugin interface in place, future capabilities become additive:

| Future capability | Plugin | Hook |
|------------------|--------|------|
| CI trigger after PR | `DevPlugin` | `OnMissionComplete` |
| Deploy to staging | `DeployPlugin` | `OnMissionComplete` |
| Slack notification | `NotifyPlugin` | `OnMissionComplete` |
| Cost budget enforcement | `BudgetPlugin` | `OnPlanReady` (reject expensive plans) |
| Dependency vulnerability scan | `SecurityPlugin` | `OnPlanReady` (inject scan phase) |
| Non-code domain tools (research, writing) | `ResearchPlugin` | `OnWorkspaceCreated` (different context) |

None of these require modifying `runTask()` or `engine.Execute()`.

---

## Decision Log

| Decision | Rationale |
|----------|-----------|
| Four hooks, not more | Maps to the four natural pipeline stages. Finer granularity (pre-phase, post-phase) belongs in the engine's executor interface, not the lifecycle plugin. |
| `PluginData` bag instead of typed returns | Plugins need cross-hook state but each plugin's state is different. A typed registry adds interface coupling without safety benefit — plugins own their keys. |
| `BasePlugin` embedding for no-op defaults | Go doesn't have optional interface methods. Embedding avoids forcing every plugin to stub four methods. |
| Review loop stays in engine, not plugin | The review loop operates at phase-execution granularity (between phases within the DAG), not at mission lifecycle granularity. Plugins configure it; the engine runs it. |
| `OnTeardown` errors never fatal | Cleanup must not mask the real mission outcome. Logging is sufficient. |
| Option C deferred, not rejected | The package split becomes a mechanical refactor once the interface exists. No architectural decision is foreclosed. |
