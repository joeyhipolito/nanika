# graph

Directed note-link graph stored in Compressed Sparse Row (CSR) format.

## Usage

```go
// Build from index link rows
g := graph.Build(links)

// Persist to disk (used by obsidian-indexer on every rebuild)
_, err := g.WriteTo(w)

// Load from disk (used by obsidian-indexer on startup)
g, err := graph.Load(r)

// Query
neighbours := g.Neighbours("daily/2026-04-21.md")
reachable  := g.BFS("daily/2026-04-21.md", 2)
```

## Wire format (CSR1)

| Field | Size |
|---|---|
| Magic `CSR1` | 4 bytes |
| Version | 1 byte |
| Node count | 4 bytes LE int32 |
| Node names (len + bytes each) | variable |
| Row-pointer count | 4 bytes LE int32 |
| Row pointers | 4 × count bytes |
| Edge count | 4 bytes LE int32 |
| Column indices | 4 × count bytes |
| CRC32-IEEE of all preceding bytes | 4 bytes |

## Integration

`obsidian-indexer` holds one `*graph.Graph` under a `sync.RWMutex`. Startup tries `Load(vault/.cache/graph.bin)` and rebuilds from the index on any error. A link-change fsnotify event triggers a 500 ms debounce, then `Build` + `WriteTo`. Stats fields: `graph_vertices`, `graph_edges`, `graph_last_rebuild` in `stats.json`.
