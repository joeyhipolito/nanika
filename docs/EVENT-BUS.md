---
produced_by: technical-writer
phase: phase-1
workspace: 20260329-0ec406b5
created_at: "2026-03-29T08:39:02Z"
confidence: high
depends_on: []
token_estimate: 1200
---

# Orchestrator Event Bus

Subscribe to orchestrator events for live monitoring and integration with external tools. Events are published as newline-delimited JSON (JSONL) and streamed over a Unix domain socket.

## Event Types

All events are typed constants. 28 event types grouped by category:

| Category | Events |
|----------|--------|
| Mission | `mission.started`, `mission.completed`, `mission.failed`, `mission.cancelled` |
| Phase | `phase.started`, `phase.completed`, `phase.failed`, `phase.skipped`, `phase.retrying` |
| Worker | `worker.spawned`, `worker.output`, `worker.completed`, `worker.failed` |
| Decompose | `decompose.started`, `decompose.completed`, `decompose.fallback` |
| Learning | `learning.extracted`, `learning.stored` |
| DAG | `dag.dependency_resolved`, `dag.phase_dispatched` |
| Role | `role.handoff` |
| Contract | `contract.validated`, `contract.violated`, `persona.contract_violation` |
| Review | `review.findings_emitted`, `review.external_requested` |
| Git | `git.worktree_created`, `git.committed`, `git.pr_created` |
| System | `system.error`, `system.checkpoint_saved` |
| Signals | `signal.scope_expansion`, `signal.replan_required`, `signal.human_decision_needed` |
| Security | `security.invisible_chars_stripped`, `security.injection_detected` |
| File | `file_overlap.detected` |

**LEARNING:** `review.findings_emitted` always fires (regardless of pass/fail) carrying all parsed blockers and warnings. Non-blocking findings and unresolved blockers at loop exhaustion are never silently discarded.

## JSONL Envelope

Every event is a JSON object on a single line, followed by a newline. Fields are:

```json
{
  "id": "evt_<8-byte-hex>",
  "type": "mission.started|phase.completed|...",
  "timestamp": "2026-03-29T08:38:39.550666Z",
  "sequence": 1,
  "mission_id": "20260329-0ec406b5",
  "phase_id": "phase-1",
  "worker_id": "technical-writer-phase-1",
  "data": { "custom": "fields", "per": "event_type" }
}
```

| Field | Type | When Present | Notes |
|-------|------|--------------|-------|
| `id` | string | always | Event UUID (`evt_` prefix + 8 hex bytes) |
| `type` | string | always | TypeScript-style event name |
| `timestamp` | string | always | RFC3339 UTC (e.g., `2026-03-29T08:38:39.550666Z`) |
| `sequence` | int64 | always | Monotonic per bus (assigned by emitter, global order) |
| `mission_id` | string | always | Mission UUID this event belongs to |
| `phase_id` | string | optional | Present for phase/worker lifecycle events |
| `worker_id` | string | optional | Present for worker lifecycle events |
| `data` | object | optional | Event-type-specific fields (varies) |

**GOTCHA:** `sequence` is assigned by the Bus (globally monotonic), not by individual emitters. This prevents collisions when concurrent missions each start at seq=1, breaking SSE replay deduplication. Mission-local sequences are preserved in JSONL logs for per-mission replay.

## Subscribe: UDS Socket

### Live Streaming

Connect to the broadcast socket at `~/.alluka/events.sock` for a live JSONL stream:

```bash
# Using socat (one-liner)
socat - UNIX-CONNECT:~/.alluka/events.sock

# Using nc (alternative)
nc -U ~/.alluka/events.sock

# Custom subscriber (Go example)
conn, _ := net.DialTimeout("unix", "~/.alluka/events.sock", 5*time.Second)
scanner := bufio.NewScanner(conn)
for scanner.Scan() {
  var ev Event
  json.Unmarshal(scanner.Bytes(), &ev)
  // process ev
}
```

The daemon (if running) listens on this socket and writes all bus events as newline-delimited JSON to every connected client.

### Paths

- **Event broadcast socket:** `~/.alluka/events.sock` — live subscribers connect here
- **Daemon PID file:** `~/.alluka/daemon.pid` — check if daemon is running
- **Daemon control socket:** `~/.alluka/daemon.sock` — orchestrator sends events here (internal use)

**GOTCHA:** `~/.alluka` is an intentional HxH reference (vessel/intelligence split). Do not rename to `~/.nanika/`.

## Subscribe: JSONL Files

### Polling with Tail

When the daemon is unavailable, tail JSONL files directly:

```bash
# Watch new events from a mission
tail -f ~/.alluka/events/<mission_id>.jsonl | jq -c '.type'

# Process all events since last check
offset=$(stat -f%z ~/.alluka/events/20260329-0ec406b5.jsonl 2>/dev/null || echo 0)
tail -c +$offset ~/.alluka/events/20260329-0ec406b5.jsonl | jq .
```

File structure:
- **Location:** `~/.alluka/events/<mission_id>.jsonl`
- **Format:** One JSON event per line (JSONL)
- **Permissions:** `0600` (user-only, mission context may be sensitive)
- **Directory:** `~/.alluka/events/`

## Examples

### Mission Lifecycle

```json
{"id":"evt_6868d2b58d433630","type":"mission.started","timestamp":"2026-03-29T08:38:39.550666Z","sequence":3,"mission_id":"20260329-0ec406b5","data":{"execution_mode":"sequential","phases":3,"task":"# Document Event Bus..."}}
{"id":"evt_6023db25e77233bb","type":"mission.completed","timestamp":"2026-03-26T10:09:02.816557Z","sequence":812,"mission_id":"20260326-05ca0d8e","data":{"artifacts":4,"duration":"32m4.767229625s","duration_seconds":1924.767230166,"execution_mode":"parallel","phase_count":5}}
```

### Phase and Worker Events

```json
{"id":"evt_d99dc9d97ecf3cc0","type":"phase.started","timestamp":"2026-03-29T08:38:39.551009Z","sequence":5,"mission_id":"20260329-0ec406b5","phase_id":"phase-1","data":{"model":"quick","name":"document-events","persona":"technical-writer","role":"planner","runtime":"claude"}}
{"id":"evt_35b76f2675a2205f","type":"worker.spawned","timestamp":"2026-03-29T08:38:40.144636Z","sequence":6,"mission_id":"20260329-0ec406b5","phase_id":"phase-1","worker_id":"technical-writer-phase-1","data":{"dir":"/Users/joeyhipolito/.alluka/workspaces/20260329-0ec406b5/workers/technical-writer-phase-1","effort_level":"low","model":"haiku","persona":"technical-writer"}}
```

### Git and Concurrency Detection

```json
{"id":"evt_9452b9030e139c7b","type":"git.worktree_created","timestamp":"2026-03-26T09:36:58.03867Z","sequence":1,"mission_id":"20260326-05ca0d8e","data":{"base_branch":"main","branch":"via/20260326-05ca0d8e/linear-issue-id-nan-95-target-repo-nanik","worktree_path":"/Users/joeyhipolito/.alluka/worktrees/20260326-05ca0d8e"}}
{"id":"evt_c96761a1fd1ad13a","type":"file_overlap.detected","timestamp":"2026-03-26T10:09:02.805309Z","sequence":792,"mission_id":"20260326-05ca0d8e","data":{"file":"plugins/nen/cmd/shu/propose.go","phases":["phase-1","phase-2","phase-3","phase-4","phase-5"],"severity":"high"}}
```

## Integration Pattern

Typical consumer pattern (e.g., nen-daemon):

1. **Probe UDS** — try to connect to `~/.alluka/events.sock`
2. **On success** — stream NDJSON events; reconnect with backoff on disconnect
3. **On failure** — fall back to JSONL polling from `~/.alluka/events/` every 5 seconds
4. **For each event** — deserialize, route to handlers based on `.type`

See `plugins/nen/cmd/nen-daemon/main.go` for a reference implementation that subscribes, routes events to scanner binaries, and persists findings to SQLite.

## Drop Detection

Both transport layers report delivery losses:

- **UDS emitter** — `.DroppedWrites()` counts socket timeouts and write failures
- **File emitter** — `.DroppedWrites()` counts I/O errors to JSONL
- **Bus** — `.SubscriberDrops()` counts slow consumers (full channel, buffered to 64 events)

**PATTERN:** If drop counts are non-zero, the event log is incomplete. Use `orchestrator metrics --mission <id>` to check phase/worker telemetry, which may be more accurate than real-time event counts.

---

**FINDING:** The bus is a fixed-capacity ring buffer (1000 events), non-blocking to publishers. Slow subscribers miss events rather than stall the mission. Use `.EventsSince(seq)` to replay buffered events on reconnect.
