---
name: memory
description: Compiled symbolic memory CLI for AI agents. Use when the user wants to store durable facts, update entity state, search prior notes without embeddings, or inspect the current known state of an entity.
allowed-tools: Bash(memory:*)
argument-hint: "[command] [args]"
keywords: memory, knowledge, recall, entity-state, search, symbolic, slots
category: productivity
version: "0.1.0"
---

# Memory - Compiled Symbolic Memory

Store and retrieve durable agent memory using an append-only log plus a compiled symbolic index.

## Install

```bash
cd plugins/memory
go build -ldflags "-s -w" -o bin/memory ./cmd/memory-cli
ln -sf $(pwd)/bin/memory ~/.alluka/bin/memory
```

## When to Use

- User wants to remember a fact for later
- User asks what is currently known about a person, project, or entity
- User wants memory search without embeddings or vector databases
- User wants durable local memory that is inspectable and rebuildable
- User wants to recompile or debug the memory index

## Configuration

Storage defaults to `~/.memory/default/`.

Environment overrides:

- `MEMORY_HOME` for the base directory
- `MEMORY_STORE` for the logical store name

## Commands

### Add

```bash
memory add "Atlas deploy owner is Alice" --entity Atlas --slot owner=Alice
memory add "Support ticket mentions rate limits" --tag team=support --source zendesk
echo "Piped note" | memory add --entity Atlas
```

### Remember

```bash
memory remember Alice --slot employer=OpenAI --slot role=Engineer
memory remember Atlas --slot owner=Platform --text "Atlas ownership confirmed"
```

### Find

```bash
memory find "deployment notes"
memory find "entity=alice"
memory find "role=engineer project=alpha" --top 10
```

Unqualified facets like `role=engineer` or `project=alpha` match the shared exact-facet index compiled from slots and tags.

Supported explicit facets:

- `entity=<name>`
- `slot.<key>=<value>`
- `tag.<key>=<value>`
- `day=YYYY-MM-DD`

### State

```bash
memory state Alice
memory state "Project Atlas" --json
```

### Log

```bash
memory log
memory log --limit 25 --json
```

### Maintenance

```bash
memory rebuild
memory doctor
memory query status --json
memory query items --json
memory query actions --json
```

## Notes

- Writes are append-only to `log.jsonl`
- Reads come from `compiled.gob`
- `memory rebuild` reconstructs the compiled index from the log
- This plugin is symbolic-first: direct entity state and lexical retrieval, no embeddings required
