# decomposer

A Claude knowledge skill that breaks complex tasks into dependency-aware PHASE lines for the Nanika orchestrator. No binary — purely a prompt-time skill loaded into Claude's context.

## What it does

When you give Claude a complex task (a feature, an article pipeline, a multi-step mission), the decomposer skill tells Claude how to produce a structured mission plan instead of a vague description. The output is a set of PHASE lines that the orchestrator executes directly — bypassing its internal LLM decomposer and giving you full, predictable control over ordering, parallelism, and persona assignment.

## Output format

Every phase is a single pipe-delimited line:

```
PHASE: <name> | OBJECTIVE: <concrete deliverable> | PERSONA: <persona> | SKILLS: <skill1,skill2> | DEPENDS: <phase1,phase2>
```

- **PHASE** — short kebab-case name (`research-frameworks`, `write-article`)
- **OBJECTIVE** — the deliverable, not the activity ("Write a 1500-word comparison" not "Write about it")
- **PERSONA** — exactly one persona from the catalog
- **SKILLS** — optional; only when the phase needs a specific CLI tool
- **DEPENDS** — optional; phases with no dependency run in parallel

## Usage

This skill activates automatically when Claude decomposes a task into a mission file. You can also invoke it explicitly:

```
decompose: "Design, implement, test, and document the notifications API"
```

Produces:

```
PHASE: design | OBJECTIVE: Design notification API endpoints, payload schemas, and delivery flow | PERSONA: architect
PHASE: implement | OBJECTIVE: Build notification handlers, storage layer, and delivery queue | PERSONA: senior-backend-engineer | DEPENDS: design
PHASE: test | OBJECTIVE: Write integration tests covering all notification endpoints and edge cases | PERSONA: qa-engineer | DEPENDS: implement
PHASE: document | OBJECTIVE: Write API reference documentation for notification endpoints | PERSONA: technical-writer | DEPENDS: implement
```

`test` and `document` have no dependency on each other — the orchestrator runs them in parallel.

## Core rules

| Rule | Summary |
|------|---------|
| One persona per phase | If work spans two specialties, split into two phases |
| Break by user value | Each phase delivers a complete capability, not a technical layer |
| 2–8 phases, prefer fewer | Aim for rich phases; single-capability tasks get 1 phase |
| Concrete objectives | State the deliverable, not the activity |
| Research before writing | Writing MUST depend on research — never parallel |
| Writing before illustration | Illustration prompts require the finished article |
| Code needs review | Add a `staff-code-reviewer` phase for implementation work |
| No phantom dependencies | Only depend on phases whose output you consume |

## Pipeline constraints

When a task matches a known pipeline (article, YouTube video), the core phase chain is locked. The decomposer reads the pipeline's `SKILL.md` before writing phases and follows its persona order exactly.

| Trigger | Pipeline skill |
|---------|---------------|
| "write an article", "blog post" | `article-pipeline` |
| "make a video", "YouTube content" | `youtube-pipeline` |

## Skill file

Full rules, worked examples, anti-patterns, and the complete persona catalog are in:

```
skills/decomposer/.claude/skills/decomposer/SKILL.md
```

## License

MIT
