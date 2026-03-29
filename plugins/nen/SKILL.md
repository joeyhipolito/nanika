---
produced_by: senior-backend-engineer
phase: phase-2
workspace: 20260323-1a3708aa
created_at: "2026-03-23T00:00:00Z"
confidence: high
depends_on: []
token_estimate: 800
---

# nen

External scanner plugin for the orchestrator's `nen` scan subsystem.
Provides three scanner binaries (`gyo`, `en`, `ryu`) that plug into
`~/.alluka/nen/scanners/` via the filesystem discovery protocol.

## Install

```bash
cd plugins/nen
bash install.sh
```

Binaries are built and copied to `~/.alluka/nen/scanners/`. The orchestrator
auto-discovers any executable in that directory on the next `nen scan` — no
registration required.

## Scanners

### gyo — orchestrator-metrics

Detects mission metric anomalies and silent worker failures.

**Ability**: `orchestrator-metrics`

**Findings**:
- `cost-anomaly` — mission cost deviates >2σ from 7-day baseline
- `duration-anomaly` — mission duration deviates >2σ
- `failure-rate-anomaly` — phase failure rate deviates >2σ
- `retry-anomaly` — retry count deviates >2σ
- `silent-failure` — `worker.failed` event without matching `phase.failed` in event log

**Data sources**: `metrics.db` (missions table), `events/<mission-id>.jsonl`

**Minimum history**: 10 missions in the past 7 days (returns no findings below this threshold)

---

### en — system-health

Checks orchestrator environment health: binary freshness, workspace hygiene,
learnings database quality, daemon socket reachability, and routing metrics.

**Ability**: `system-health`

**Findings**:
- `binary-freshness` — orchestrator binary age (info/low/medium by threshold)
- `workspace-hygiene` — stale workspaces older than 7 days
- `embedding-coverage` — fraction of learnings with embeddings in `learnings.db`
- `dead-weight` — learnings that match archival criteria (unused, low-compliance, etc.)
- `daemon-health` — whether the daemon socket is reachable
- `routing-quality` — fallback routing rate from `metrics.db`
- `mission-activity` — days since last mission

**Data sources**: `metrics.db`, `learnings.db`, `workspaces/`, `daemon.sock`

---

### ryu — cost-analysis

Surfaces cost trends, model efficiency gaps, retry waste, and minimal-output phases.

**Ability**: `cost-analysis`

**Findings**:
- `cost-trend` — second half of 7-day window costs ≥50% more than first half
- `model-efficiency` — Opus phases cost >2× more per phase than Sonnet phases
- `retry-waste` — estimated dollar cost attributed to retried phases
- `output-waste` — completed phases with <200 chars output that consumed meaningful cost
- `retry-events` — aggregated `phase.retrying` event count from event logs

**Data sources**: `metrics.db` (missions + phases tables), `events/*.jsonl`

---

## Scanner Protocol

Each binary implements the nen external scanner protocol:

```
stdin:   (nothing)
flags:   --scope <JSON>    # {"kind":"...","value":"..."} — may be empty {}
stdout:  []Finding JSON    # one JSON array, newline-terminated
stderr:  error messages    # non-fatal; orchestrator logs and continues
exit:    0 on success, non-zero on fatal startup error
```

The orchestrator collects stdout, parses `[]Finding`, persists to the findings
database, and continues even if individual scanners fail.

## Writing a New Scanner

1. Create `cmd/<name>/main.go` in this module.
2. Parse `--scope` with `flag.StringVar` and `json.Unmarshal`.
3. Import `github.com/joeyhipolito/nen/internal/scan` for shared types and DB helpers.
4. Write findings to stdout as `json.Marshal([]scan.Finding{...})`.
5. Add the binary to `install.sh`.

```go
package main

import (
    "context"
    "encoding/json"
    "flag"
    "fmt"
    "os"

    "github.com/joeyhipolito/nen/internal/scan"
)

func main() {
    var scopeJSON string
    flag.StringVar(&scopeJSON, "scope", "{}", "JSON-encoded scan scope")
    flag.Parse()

    var scope scan.Scope
    if err := json.Unmarshal([]byte(scopeJSON), &scope); err != nil {
        fmt.Fprintf(os.Stderr, "myScanner: invalid --scope: %v\n", err)
        os.Exit(1)
    }

    findings := []scan.Finding{
        // ... your scan logic
    }

    out, _ := json.Marshal(findings)
    fmt.Println(string(out))
}
```

## internal/scan Package

Shared utilities available to all scanner binaries:

| Function | Description |
|---|---|
| `scan.Dir()` | Returns `~/.alluka` config directory (same priority chain as orchestrator) |
| `scan.MetricsDBPath()` | Path to `metrics.db` |
| `scan.LearningsDBPath()` | Path to `learnings.db` |
| `scan.OpenMetricsDB()` | Open `metrics.db` in WAL read mode |
| `scan.OpenLearningsDB()` | Open `learnings.db` read-only (nil if missing) |
| `scan.OpenReadOnly(path)` | Open any SQLite file read-only (nil if missing) |
| `scan.EventsDir()` | Path to `events/` directory |
| `scan.DaemonSocketPath()` | Path to `daemon.sock` |
| `scan.CountPhaseRetryingEvents(ctx, path, since)` | Count `phase.retrying` events in a JSONL log |
| `scan.FindSilentFailures(logPath)` | Phase IDs with `worker.failed` but no `phase.failed` |

## LEARNING: Scanner discovery is zero-config

Drop any executable into `~/.alluka/nen/scanners/` and it runs automatically
on the next `orchestrator nen scan`. The orchestrator checks the executable bit;
no manifest or registration is needed.

## PATTERN: Non-fatal scan errors

Scanners should write partial findings to stdout even when some sub-checks fail.
Write error details to stderr. The orchestrator embeds stderr in `ScanResult.Err`
and continues collecting findings from other scanners.
