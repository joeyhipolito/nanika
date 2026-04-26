# index — SQLite note + link-graph index

## Schema

```sql
schema_version  (version INTEGER)
notes           (path TEXT PK, title TEXT, mod_time INTEGER)
links           (src TEXT → notes(path) ON DELETE CASCADE, dst TEXT, PK(src,dst))
```

Three indexes: `idx_links_src`, `idx_links_dst`, `idx_notes_mod_time`.

## Upsert contract

`Upsert(path, NoteMeta, links)` is atomic:

1. INSERT OR REPLACE the note row.
2. DELETE all existing `links` rows where `src = path`.
3. INSERT the new link set.

Steps 1–3 run inside a single transaction. Readers never see a partial link state.

`ReplaceLinks` does steps 2–3 only (does not touch the note row).

## Cascade rule

Deleting a note removes its **outgoing** link rows via `ON DELETE CASCADE` on `links.src`.
Incoming link rows (`links.dst = path`) are **preserved** — callers use them to detect dangling references.

## Downstream callers

- **TRK-550** — Indexer consumer: uses `Upsert` + `Neighbours` for incremental vault sync.
- **TRK-551** — Link-graph query layer: uses `Neighbours` + `CountNotes` for graph traversal.
