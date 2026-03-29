---
name: decomposer
description: Mission decomposition skill — breaks complex tasks into dependency-aware PHASE lines that the orchestrator executes directly, bypassing its internal LLM decomposer. Use when planning missions, creating mission files, or decomposing complex tasks.
license: MIT
metadata:
  author: joey
  version: "1.1.0"
---

# Mission Decomposition

Break complex tasks into PHASE lines that the Nanika orchestrator executes directly.
When a mission file contains PHASE lines, the orchestrator skips its internal decomposer
and runs your phases as-is. This gives you full control over ordering, dependencies,
persona selection, and parallelism.

## Output Format

Every phase is a single pipe-delimited line:

```text
PHASE: <name> | OBJECTIVE: <what to accomplish> | PERSONA: <persona-name> | SKILLS: <skill1,skill2> | DEPENDS: <phase-name1,phase-name2>
```

- `PHASE` — short kebab-case name such as `research-options` or `review-auth-flow`
- `OBJECTIVE` — a concrete deliverable, not a vague activity
- `PERSONA` — exactly one persona from the catalog below
- `SKILLS` — optional, comma-separated; include only when the phase genuinely needs a tool
- `DEPENDS` — optional, comma-separated phase names; omit for independent work

## Core Rules

### Rule 1: One Persona Per Phase
Each phase gets exactly one persona. If work spans specialties, split it.

```text
BAD:  PHASE: build-auth | OBJECTIVE: Design and implement auth | PERSONA: architect
GOOD: PHASE: design-auth | OBJECTIVE: Design the auth flow and token contract | PERSONA: architect
      PHASE: implement-auth | OBJECTIVE: Build the auth handlers and persistence layer | PERSONA: senior-backend-engineer | DEPENDS: design-auth
```

### Rule 2: Break by User Value, Not Technical Layer
Each phase should deliver a meaningful capability, not a thin layer.

```text
BAD:  PHASE: setup-db | OBJECTIVE: Create tables | PERSONA: senior-backend-engineer
GOOD: PHASE: implement-auth | OBJECTIVE: Build auth with schema, handlers, and tests | PERSONA: senior-backend-engineer
```

### Rule 3: Keep Plans Tight
Aim for 2-8 phases. Maximum 12. Single-capability tasks may only need 1-2 phases.

### Rule 4: Objectives Must Be Concrete
State the actual deliverable.

```text
BAD:  OBJECTIVE: Research the topic
GOOD: OBJECTIVE: Compare three deployment options with operational trade-offs and a recommendation
```

### Rule 5: Pick the Right Persona Boundary
Use the narrowest current persona that fits.

- `academic-researcher` only for scholarly, literature-backed, citation-heavy, or methodology-sensitive research
- `architect` for technical comparison, system trade-offs, framework selection, and design decisions
- `data-analyst` for logs, metrics, usage, trends, and quantitative before/after analysis
- `technical-writer` for docs, developer explainers, ADRs, and article-style technical synthesis
- `senior-backend-engineer` for backend implementation, CLIs, APIs, storage, and service code
- `senior-frontend-engineer` for React, Next.js, TypeScript, and UI work
- `staff-code-reviewer`, `security-auditor`, and `qa-engineer` stay in review/test/audit lanes

### Rule 6: Assign Skills Only When Needed
Do not guess skill names. Omit `SKILLS` entirely when no skill is required.

### Rule 7: Independent Work Runs in Parallel
If two phases do not consume each other, omit `DEPENDS` so the orchestrator can run them concurrently.

```text
PHASE: analyze-logs | OBJECTIVE: Identify the endpoints with the highest 500 rate from the last 7 days | PERSONA: data-analyst
PHASE: review-auth | OBJECTIVE: Review the auth middleware for correctness and security regressions | PERSONA: staff-code-reviewer
PHASE: summarize | OBJECTIVE: Produce a rollout recommendation from the log analysis and code review findings | PERSONA: architect | DEPENDS: analyze-logs, review-auth
```

### Rule 8: Respect Data Dependencies
If a phase needs another phase's output, it MUST declare `DEPENDS`.

### Rule 9: Code Implementation Normally Requires Review
When a mission includes implementation on a repo-backed target, add a downstream `staff-code-reviewer` phase by default.
Use `security-auditor` instead only when security is the primary concern.

### Rule 10: Do Not Pad the Plan
No filler phases. Add only the work needed to reach the deliverable, plus the required review phase for implementation.

## Dependency Rules

### Research Before Synthesis
If a writing or architecture phase depends on gathered evidence, it must depend on the research phase.

```text
BAD:  PHASE: research | OBJECTIVE: Gather sources | PERSONA: academic-researcher
      PHASE: write | OBJECTIVE: Draft the explainer | PERSONA: technical-writer

GOOD: PHASE: research | OBJECTIVE: Gather sources | PERSONA: academic-researcher
      PHASE: write | OBJECTIVE: Draft the explainer | PERSONA: technical-writer | DEPENDS: research
```

### Design Before Implementation
If architecture or interface decisions are required first, implementation must depend on the design phase.

### Review After Implementation
Review, QA, or security audit phases that inspect code changes must depend on the implementation phase.

## Handoff Chains

When a persona clearly hands off to another, consider adding a follow-up phase.
Only add it when the task benefits from the chain.

| From | To | When |
|------|----|------|
| academic-researcher | technical-writer | Research must become docs, an explainer, or article-style technical synthesis |
| academic-researcher | architect | Research informs a technical decision or recommendation |
| academic-researcher | senior-backend-engineer | Research findings should be implemented |
| architect | senior-backend-engineer | Backend implementation follows the design |
| architect | senior-frontend-engineer | Frontend implementation follows the design |
| senior-backend-engineer | staff-code-reviewer | Code changes should be reviewed |
| senior-backend-engineer | qa-engineer | Implementation needs dedicated test expansion |
| senior-frontend-engineer | qa-engineer | UI changes need focused validation or regression coverage |
| security-auditor | senior-backend-engineer | Findings need remediation |
| data-analyst | technical-writer | Analysis must become a readable report or explainer |

## Available Personas

| Persona | Best For |
|---------|----------|
| academic-researcher | Scholarly research, evidence synthesis, literature review, study critique |
| architect | Technical comparisons, trade-off analysis, architecture, interfaces, system design |
| data-analyst | Logs, metrics, trends, usage analysis, reproducible quantitative summaries |
| devops-engineer | CI/CD, Docker, releases, deployment automation, health checks, observability |
| qa-engineer | Test strategy, regression coverage, fixtures, validation, flaky-test diagnosis |
| security-auditor | Threat modeling, vulnerability review, auth/secret handling, trust-boundary analysis |
| senior-backend-engineer | Backend implementation, CLIs, APIs, storage, migrations, service code |
| senior-frontend-engineer | React/Next.js/TypeScript UI implementation and accessibility fixes |
| staff-code-reviewer | Code review, hidden regression detection, API compatibility, maintainability review |
| technical-writer | READMEs, docs, ADRs, guides, technical explainers, article-style synthesis |

## Anti-Patterns

### Using `academic-researcher` as the Generic Research Bucket
Framework comparison, API option analysis, and implementation-path research are not automatically academic work.
Use `architect` or `data-analyst` unless the task explicitly needs literature, citations, or methodology scrutiny.

### Collapsing Design and Build Into One Phase
If the task requires real architectural decisions, split the design from the implementation.

### Sending Writing Tasks to Missing Creative Personas
The active catalog does not include narrative-only writer personas. Use `technical-writer` for technical communication and explainers.

### Parallelizing Dependent Phases
If phase B consumes phase A's output, add `DEPENDS`. Silent dependency loss creates bad execution order.

### Defaulting Every Coding Task to Backend
Use `senior-frontend-engineer` for React/Next.js/TypeScript work and `devops-engineer` for deployment automation.

## Worked Examples

### Technical Research -> Recommendation
**Task**: "Compare options for storing embeddings locally and recommend one for the orchestrator"

```text
PHASE: compare-storage-options | OBJECTIVE: Compare SQLite, Postgres, and file-backed vector storage for local embeddings with trade-offs and recommendation criteria | PERSONA: architect
PHASE: write-adr | OBJECTIVE: Write an ADR documenting the selected storage approach, trade-offs accepted, and migration implications | PERSONA: technical-writer | DEPENDS: compare-storage-options
```

### Academic Research -> Technical Synthesis
**Task**: "Review the literature on pair programming productivity and turn it into a technical explainer"

```text
PHASE: review-literature | OBJECTIVE: Gather and synthesize the strongest studies on pair programming productivity, methodology quality, and conflicting findings | PERSONA: academic-researcher
PHASE: write-explainer | OBJECTIVE: Write a developer-facing explainer that summarizes the evidence, caveats, and practical takeaways | PERSONA: technical-writer | DEPENDS: review-literature
```

### Design -> Build -> Review
**Task**: "Design and implement a plugin loading system for the CLI"

```text
PHASE: design-plugin-system | OBJECTIVE: Define the plugin discovery model, interfaces, lifecycle, and compatibility constraints | PERSONA: architect
PHASE: implement-plugin-system | OBJECTIVE: Build plugin discovery, loading, and validation for the CLI according to the design | PERSONA: senior-backend-engineer | DEPENDS: design-plugin-system
PHASE: review-plugin-system | OBJECTIVE: Review the implementation for correctness, regressions, and maintainability | PERSONA: staff-code-reviewer | DEPENDS: implement-plugin-system
```

### Implementation -> QA
**Task**: "Fix the flaky HTTP handler tests and improve coverage for edge cases"

```text
PHASE: stabilize-tests | OBJECTIVE: Fix the flaky handler behavior and any production code issues causing nondeterministic failures | PERSONA: senior-backend-engineer
PHASE: expand-coverage | OBJECTIVE: Add focused regression coverage for the flaky cases, edge inputs, and error paths | PERSONA: qa-engineer | DEPENDS: stabilize-tests
PHASE: review-test-fix | OBJECTIVE: Review the fixes and tests for hidden regressions or maintainability issues | PERSONA: staff-code-reviewer | DEPENDS: expand-coverage
```
