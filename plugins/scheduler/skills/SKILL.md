---
name: scheduler
description: Schedules and runs cron jobs and the nanika publishing pipeline via scheduler CLI. Use when scheduling jobs, managing cron tasks, running the daemon, checking job history, or setting up the daily publishing pipeline.
allowed-tools: Bash(scheduler:*)
argument-hint: "[job-id]"
keywords: scheduler, cron, jobs, daemon, pipeline, automation, history
category: productivity
version: "1.1.0"
---

# Scheduler — Cron Job Runner + Publishing Pipeline

Manages recurring shell jobs with cron expressions. Powers the nanika publishing pipeline. All state is persisted in SQLite. The default DB path is `~/.alluka/scheduler/scheduler.db`; the active path is set by `db_path` in `~/.alluka/scheduler/config` (run `scheduler configure show` to check).

## When to Use

- User wants to schedule a recurring shell command or script
- User wants to run the daemon that executes scheduled jobs
- User wants to set up or modify the nanika publishing pipeline
- User asks about cron jobs, periodic tasks, or automation
- User wants to view execution history or check job logs
- User wants to verify their scheduler setup is working
- User wants to enable, disable, or run a job immediately

## Commands

### Doctor

```bash
# Verify complete installation (CLIs, DB, daemon status)
scheduler doctor
scheduler doctor --json
```

### Daemon

The daemon polls every 30 seconds and executes any job whose `next_run_at` has passed. It writes a JSON event to `~/.alluka/events/scheduler.jsonl` after every job run.

```bash
# Start the daemon (foreground, polls every 30s)
scheduler daemon

# Start with orchestrator socket notifications
scheduler daemon --notify

# Run one tick and exit (useful for testing)
scheduler daemon --once

# Stop a running daemon
scheduler daemon --stop
```

**Production tip:** Run the daemon in the background with your process supervisor of choice. A minimal launchd approach:

```bash
# Run in background (redirect logs)
scheduler daemon >> ~/.alluka/logs/scheduler.log 2>&1 &
```

### Init — Scheduler Database

Initializes the scheduler database. Scheduler itself does NOT register any
cross-plugin cron jobs — each plugin owns its own jobs via its own init
command. For example, `shu propose --init` (from the nen plugin) registers
the nen self-improvement loop jobs (propose-remediations, dispatch-approved,
close-sweep, evaluate-weekly).

```bash
# Initialize the scheduler database
scheduler init

# (--force is reserved for future use when scheduler-owned jobs exist)
scheduler init --force
```

If you want the nanika publishing pipeline (`scout gather`, `engage scan +
draft`, weekly `scout intel`), install the `scout` and `engage` plugins and
add the jobs manually via `scheduler jobs add`, or use whatever init
command those plugins ship.

After running `scheduler init`, start the daemon to activate any jobs
registered by other plugins:

```bash
scheduler init
shu propose --init      # optional — registers nen loop jobs
scheduler daemon
```

### Jobs

```bash
# List all cron jobs
scheduler jobs

# Add a recurring job
scheduler jobs add --name "daily-backup" --cron "0 2 * * *" --command "tar czf /tmp/backup.tgz ~/docs"
scheduler jobs add --name "health-check" --cron "*/5 * * * *" --command "curl -s localhost:8080/health"

# Add with timeout (seconds)
scheduler jobs add --name "slow-job" --cron "0 1 * * *" --command "heavy-script.sh" --timeout 3600

# Enable/disable a job
scheduler jobs enable <job-id>
scheduler jobs disable <job-id>

# Remove a job (cascades to run history)
scheduler jobs remove <job-id>
```

### Run

Run a job immediately, ignoring its schedule.

```bash
scheduler run <job-id>
```

### Logs

View execution output for a specific job.

```bash
scheduler logs <job-id>
scheduler logs <job-id> --limit 10
```

### Status

Overview of daemon state, job counts, and next scheduled run.

```bash
scheduler status
scheduler status --json
```

### History

Shows the most recent job run events from `~/.alluka/events/scheduler.jsonl`, newest first.

```bash
# Show last 50 events (default)
scheduler history

# Show last N events
scheduler history --limit 20
```

Output columns: `TIME | STATUS | JOB | EXIT | DURATION | STDERR`

- `STATUS` is `ok` for exit code 0, `FAILED` otherwise
- `STDERR` is truncated to 40 chars for readability

### Query

JSON-native subcommands for agent use.

```bash
# Daemon state, job counts, and next scheduled run
scheduler query status --json

# List all jobs with schedule and last-run details
scheduler query items --json

# Run a job immediately via query protocol
scheduler query action run <job-id> --json

# Enable or disable a job via query protocol
scheduler query action enable <job-id> --json
scheduler query action disable <job-id> --json

# List available actions
scheduler query actions --json
```

## Plugin Ownership of Cron Jobs

Scheduler provides the execution infrastructure (daemon, cron parsing, DB,
history). It does **not** ship default jobs for other plugins. Each plugin
that wants recurring behavior registers its own jobs via its own init
command. This keeps cross-plugin dependencies explicit and prevents
install-set drift (if you don't have `engage` installed, nothing tries to
run `engage scan`).

**Example — the nen self-improvement loop** registers four jobs via its
own init command:

```bash
shu propose --init
```

Registers:

| Job | Schedule | Command |
|---|---|---|
| `propose-remediations` | every 4h | `shu propose --json` |
| `dispatch-approved` | every 15m | `shu dispatch --max-concurrent 1 --max-per-hour 6` |
| `close-sweep` | every 15m | `shu close --sweep --json` |
| `evaluate-weekly` | Mondays 10am | `ko evaluate-proposals --json` |

**Adding your own jobs — the manual path:**

```bash
scheduler jobs add --name "daily-backup" --cron "0 2 * * *" --command "tar czf /tmp/backup.tgz ~/docs"
scheduler jobs add --name "health-check" --cron "*/5 * * * *" --command "curl -s localhost:8080/health"
```

**Starting the daemon:**

```bash
scheduler init           # one-time DB setup
shu propose --init       # optional — registers nen loop jobs
scheduler daemon         # activates the schedule
scheduler jobs           # verify jobs are registered
scheduler history        # after running for a while
```

## Configuration

Config file: `~/.alluka/scheduler/config` (key=value format)

```
db_path = /Users/<you>/.alluka/scheduler/scheduler.db
log_level = info
shell = /bin/sh
max_concurrent = 4
```

Run `scheduler configure` to create or update it interactively. All keys are optional — missing keys use the defaults above.

> **DB path note:** Installations created with `scheduler-cli configure` (the old binary name) may have `db_path` pointing to the legacy location `~/.scheduler/scheduler.db`. Run `scheduler configure show` to see the active path. To migrate to the canonical location, update `db_path` to `~/.alluka/scheduler/scheduler.db` and copy the database file.

## Cron Expression Reference

```
┌─── minute (0–59)
│ ┌─── hour (0–23)
│ │ ┌─── day of month (1–31)
│ │ │ ┌─── month (1–12)
│ │ │ │ ┌─── day of week (0–7, 0=Sunday, 1=Monday)
* * * * *

*/5 * * * *     every 5 minutes
0 * * * *       every hour at :00
0 8 * * *       daily at 8 AM
0 10 * * 1      Monday at 10 AM
0 2 * * 0       weekly on Sunday at 2 AM
```

## Event Log

Every job run appends a JSON line to `~/.alluka/events/scheduler.jsonl`:

```json
{"type":"schedule.completed","job_id":1,"job_name":"daily-scout","command":"scout gather","duration_ms":4201,"exit_code":0,"ts":"2026-03-25T08:00:04Z"}
{"type":"schedule.failed","job_id":2,"job_name":"daily-engage","command":"engage scan && engage draft --reschedule-post","duration_ms":312,"exit_code":1,"stderr":"connection refused","ts":"2026-03-25T09:00:00Z"}
```

Use `scheduler history` to view this log in a readable tabular format, or tail it directly:

```bash
tail -f ~/.alluka/events/scheduler.jsonl | jq .
```

## Examples

**User**: "schedule a daily email digest at 8am"
**Action**: `scheduler jobs add --name "daily-digest" --cron "0 8 * * *" --command "gmail inbox --unread --json"`

**User**: "check what scheduled jobs are running"
**Action**: `scheduler jobs`

**User**: "run the nightly backup job now"
**Action**: `scheduler query action run <job-id>`

**User**: "show the scheduler logs"
**Action**: `scheduler logs`

## Build

```bash
cd plugins/scheduler
go build -ldflags "-s -w" -o bin/scheduler ./cmd/scheduler-cli
ln -sf $(pwd)/bin/scheduler ~/.alluka/bin/scheduler
```
