# Persona Standard

The definitive reference for creating, modifying, and auditing Nanika personas. One document, every convention.

**TL;DR**: A persona is a markdown file at `personas/{name}.md` that defines a worker's identity, expertise, and routing triggers. The orchestrator injects the full file as the first section of each worker's CLAUDE.md. Routing uses Haiku (LLM) with keyword fallback scored from `## When to Use` and `## When NOT to Use` sections. Every `hand off to` reference must name an exact existing persona filename.

## 1. Architecture Overview

Nanika personas use a two-layer injection model:

| Layer | Mechanism | When |
|-------|-----------|------|
| Identity injection | Full `Content` → worker CLAUDE.md | Every phase execution |
| Routing catalog | `FormatForDecomposer()` summary → Haiku | Every phase assignment |
| Keyword fallback | `scoreKeywords()` from WhenToUse/WhenNotUse | When Haiku unavailable |
| Handoff graph | `HandsOffTo[]` from "hand off to X" patterns | Shown to decomposer for chaining |
| Memory | `personas/{name}/MEMORY.md` → worker context | Seeded before session, merged back after |

### How Routing Works

1. **LLM match (primary)**: `FormatForDecomposer()` builds a catalog summary (name + title + WhenToUse triggers + HandsOffTo) and sends it to Haiku with the task description. Haiku returns a single persona name.

2. **Keyword match (fallback)**: Scores each persona against the task:
   - `+1` per WhenToUse word that matches (prefix match, min 4 chars)
   - `-1` per WhenNotUse word that matches (min 6 chars, avoids common words)
   - `+3` if the persona name stem appears in the task text
   - Alphabetically first persona as deterministic fallback when all scores are 0

### Parsing Mechanics

The Go code in `internal/persona/personas.go` extracts these sections:

```
extractSection(content, "## When to Use")     → WhenToUse[]     (bullet items only)
extractSection(content, "## When NOT to Use") → WhenNotUse[]    (bullet items only)
extractSection(content, "## Learning Focus")  → LearningFocus[] (bullet items only)
extractHandoffs(WhenNotUse, catalog)          → HandsOffTo[]    (regex: "hand off to ([\w][\w-]*)")
```

The first line of the file → `Title`. The full file → `Content`. The filename minus `.md` → `Name`.

### Who Uses Personas

| Consumer | Loading | What It Uses |
|----------|---------|-------------|
| Orchestrator | Full catalog + LLM/keyword matching | Everything |
| Engage CLI | `LoadVoice(name)` — reads file by name | `Content` only |
| Publish CLI | `LoadPersona(path)` — reads file by path | `Content` only |

## 2. File Specification

**Location**: `~/nanika/personas/{name}.md` (flat file) or `~/nanika/personas/{name}/{name}.md` (directory convention)

**Naming**: Kebab-case. Must match the identity in the content. Filename = persona identity everywhere (PHASE lines, `--persona` flags, metrics, memory).

### Required Sections

| Section | Parsed? | Rules |
|---------|---------|-------|
| `# {Name} — {Tagline}` | Yes → `Title` | Max 72 chars. Tagline = what this persona does. |
| `## Identity` | No (content only) | First-person. 3-4 sentences. Who you are, who you work for, your constraints. |
| `## Goal` | No | One sentence. The single deliverable this persona produces. |
| `## Expertise` | No | Bullet list, 6-10 items. Concrete skills, not generic qualities. |
| `## When to Use` | Yes → `WhenToUse[]` | 4-8 bullets. See §3 for quality criteria. |
| `## When NOT to Use` | Yes → `WhenNotUse[]`, `HandsOffTo[]` | Every bullet must name an alternate persona. See §4. |
| `## Principles` | No | Domain-specific. Alternative headings OK: "Core Techniques", "Article Modes". |
| `## Anti-Patterns` | No | 4-8 bullets. What NOT to do. |
| `## Methodology` | No | Numbered steps. The persona's workflow. |
| `## Output Format` | No | Template or format spec showing actual output structure. |
| `## Self-Check` | No | Checkbox list. Quality gates the persona verifies before completing. |

### Optional Sections

| Section | Parsed? | When to Include |
|---------|---------|----------------|
| `## Learning Focus` | Yes → `LearningFocus[]` | Technical personas that accumulate domain knowledge |
| `## In-Mission Note Capture` | No | Content-producing personas (shows `obsidian capture` patterns) |
| `## Examples` | No | When input/output examples clarify the persona's work |

## 3. WhenToUse Quality Criteria

The `## When to Use` section drives both LLM and keyword routing. Quality here directly determines whether the right persona gets assigned.

**Rules:**

1. **4-8 bullets.** Fewer fails to differentiate. More creates noise in the Haiku prompt.
2. **One trigger per bullet.** Don't combine: "Research and implementation" is two triggers.
3. **Each bullet must contain at least one distinctive word (≥6 chars).** The keyword scorer ignores words shorter than 4 chars. Words under 6 chars risk false matches.
4. **Use domain-specific vocabulary.** "Implement" is too broad. "Implementing Go endpoints" is specific.
5. **Avoid overlapping vocabulary across personas.** If two personas share many WhenToUse words, keyword matching cannot differentiate them. The LLM handles ambiguity; keywords cannot.

**Good:**
```markdown
- Greenfield system design or major feature additions
- Choosing between storage engines, protocols, or architectural patterns
- Designing plugin/extension systems
```

**Bad:**
```markdown
- Helping with tasks
- When the user needs something done
- General coding work
```

## 4. Handoff Contract

Every bullet in `## When NOT to Use` should follow this pattern:

```
- {Task description} (hand off to {exact-persona-filename})
```

The regex `hand off to ([\w][\w-]*)` extracts the target name. The target must exist in the persona catalog.

**Valid:**
```markdown
- Implementing code (hand off to senior-backend-engineer)
- System design (hand off to architect)
- Writing production code (hand off to senior-backend-engineer or senior-frontend-engineer)
```

**Invalid (parser will not extract):**
```markdown
- Implementing code (hand off to the relevant engineer)     # extracts "the"
- DevOps work (hand off to devops)                           # no persona named "devops"
- Social media (hand off to social channels)                 # not a persona
```

When no appropriate persona exists for a handoff, omit the `hand off to` clause entirely and describe the boundary:
```markdown
- Work task management (use todoist CLI directly, not a persona task)
```

## 5. MEMORY.md

Each persona can have a persistent memory file at `personas/{name}/MEMORY.md`. This file is:
- **Seeded** into the worker's Claude auto-memory before each session
- **Merged back** after the session (new lines appended, deduplication by line)

### Health Criteria

| Criteria | Rule |
|----------|------|
| Domain relevance | Every entry must help THIS persona do its job better. Scout CLI config does not belong in senior-backend-engineer memory. |
| Size | Target <5KB. Above 10KB, trim older entries or split into topic files. |
| Content type | Patterns, gotchas, and decisions — not raw task output or mission logs. |
| Cross-contamination | If a MEMORY.md contains knowledge from another persona's domain, it was accumulated when this persona ran a task outside its scope. Remove it. |

### Audit Checklist

```
□ Every entry is domain-relevant to this persona's expertise
□ Size is under 10KB
□ No entries from other personas' domains
□ Entries are patterns/gotchas, not raw task output
```

## 6. Checklists

### New Persona

```
□ 1. Create personas/{name}.md following §2 section order
□ 2. Filename = kebab-case, matches identity in content
□ 3. Title line (# Name — Tagline) under 72 characters
□ 4. WhenToUse has 4-8 bullets, each with ≥1 word ≥6 chars
□ 5. Every WhenNotToUse bullet names an exact existing persona filename
□ 6. Create personas/{name}/ directory with empty MEMORY.md
□ 7. Add persona to personaColor() map in daemon/api.go
□ 8. Run: go test ./internal/persona/... (TestLoad, TestHandoffIntegrity, TestSectionCompliance)
□ 9. Test keyword match: does keywordMatch("{representative task}") return this persona?
```

### Rename Persona

```
□ 1. Rename the .md file
□ 2. Rename the MEMORY.md directory if it exists
□ 3. Update decompose.go hardcoded fallbacks (search for old name)
□ 4. Update personas_test.go TestGet table (old name → false, new name → true)
□ 5. Update personaColor() map in daemon/api.go
□ 6. Update ALL "hand off to {old-name}" references across every other persona file
□ 7. Run: go test ./internal/persona/... AND ./internal/decompose/...
```

### MEMORY.md Audit

```
□ 1. Read the file — is every entry about this persona's domain?
□ 2. Remove entries that belong to other personas' domains
□ 3. Remove raw mission logs and implementation details
□ 4. Keep patterns, gotchas, and reusable decisions
□ 5. Verify size is under 10KB
```

## 7. Quality Metrics

These metrics can be measured from existing infrastructure to track persona system health over time.

| Metric | Source | How to Measure |
|--------|--------|----------------|
| Phase failure rate per persona | `~/.alluka/metrics.jsonl` | Failed phases / total phases per persona |
| Stale name usage | Event logs | Phases using persona names not in current catalog |
| Handoff validity rate | `TestHandoffIntegrity` | 0 failures = 100% valid |
| Section compliance | `TestSectionCompliance` | 0 failures = 100% compliant |
| Keyword match accuracy | `TestKeywordMatch_PersonaSelection` | Pass rate on representative tasks |
| MEMORY.md health | Filesystem | Size distribution, cross-contamination check |

### Running the Test Suite

```bash
cd ~/skills/orchestrator

# Full persona test suite (includes handoff integrity + section compliance)
go test ./internal/persona/... -v

# Decompose tests (includes persona assignment in keyword decomposer)
go test ./internal/decompose/... -v
```

## 8. Conventions

### Persona Types

| Type | Example | Size Range | Key Feature |
|------|---------|-----------|-------------|
| Technical | senior-backend-engineer, senior-golang-engineer, senior-rust-engineer, architect | 6-10KB | Learning Focus section, code output templates |
| Creative | storyteller, narrator, artist | 13-35KB | Named techniques, style references, mode-specific templates |
| Analytical | data-analyst, academic-researcher | 6-10KB | Methodology with reproducible queries, evidence standards |
| Meta | methodologist, indie-coach | 8-11KB | Process guidance, framework references |

### Persona vs. Skill

| | Persona | Skill |
|-|---------|-------|
| Purpose | Defines HOW to think and write | Defines HOW to use a tool |
| Loaded | Injected into worker CLAUDE.md | On-demand by Claude Code |
| Matching | LLM + keyword from WhenToUse | YAML frontmatter `description` |
| Memory | Per-persona MEMORY.md accumulation | None |
| Format | Markdown with YAML frontmatter (see §9) | Markdown with YAML frontmatter |

## 9. YAML Frontmatter

All persona files carry YAML frontmatter above the `# Title` line. The orchestrator reads this at load time to populate routing metadata without parsing prose sections.

```yaml
---
role: implementer          # planner | implementer | reviewer
capabilities:
  - Go development
  - HTTP servers and middleware
triggers:
  - implement
  - build
  - backend
handoffs:
  - architect
  - senior-frontend-engineer
output_requires:           # optional — list of section headers required in output
  - "### Blockers"
  - "### Warnings"
---
```

### Fields

| Field | Required | Values | Used By |
|-------|----------|--------|---------|
| `role` | Yes | `planner`, `implementer`, `reviewer` | Orchestrator phase categorization |
| `capabilities` | Yes | List of concrete skills | Routing catalog, decomposer hints |
| `triggers` | Yes | Short keyword list | Keyword fallback scorer (supplements `## When to Use`) |
| `handoffs` | Yes | List of persona filenames | Handoff graph shown to decomposer |
| `output_requires` | No | List of required section headers | Output validation for structured-output personas |

### Notes

- `triggers` are short (1–3 word) keywords used by the keyword scorer when Haiku is unavailable. They complement `## When to Use` bullets — keep them terse and distinct.
- `output_requires` is only needed for personas whose output must contain specific sections (e.g., reviewers that must emit `### Blockers` and `### Warnings`).
- The `handoffs` list must exactly match filenames in `personas/` (no `.md` extension). Run `TestHandoffIntegrity` to verify.
- The `role` field is informational — the orchestrator uses it for phase categorization and dashboard coloring (see `personaColor()` in `daemon/api.go`).

---

*Updated Mar 29, 2026. Added §9 YAML frontmatter convention; corrected §8 Persona vs. Skill table.*
