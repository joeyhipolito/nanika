# recall

Scored BFS recall over the Obsidian vault link graph.

## Overview

Given a seed note path, `recall` traverses the link graph up to `MaxHops` steps
and returns the top-K neighbours ranked by a three-signal score:

| Signal | Formula | Effect |
|--------|---------|--------|
| Proximity | `1 / (hop + 1)` | Closer nodes score higher |
| Recency | `1 / (1 + age_days)` | Recently modified notes score higher |
| Folder prior | `+1.0` if same folder as seed | Co-located notes get a boost |

## Packages

- **`walker.go`** — `Walker` / `Walk`: BFS traversal with `VisitedCap` and scoring.
- **`recall.go`** — `Run`: thin wrapper that calls `Walker.Walk` and also detects
  dangling links (edges pointing to notes not in the corpus).
- **`engine.go`** — `Engine`: wires a graph closure + `*index.Indexer` for use as
  an RPC callback. Loads docs from the index on each call.
- **`types.go`** — `Document`, `ScoredDoc`, `Request`, `WalkResult`.

## Usage

```go
engine := recall.NewEngine(graphFn, idxr)

results, err := engine.Recall(recall.Request{
    Seed:    "daily/2026-04-21.md",
    MaxHops: 2,
    Limit:   5,
})
```

## Dangling Links

`Run` returns dangling-link errors alongside results. The engine silently drops
them — callers that care should call `Run` directly.
