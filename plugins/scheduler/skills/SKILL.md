---
name: scheduler
description: Schedules and runs cron jobs via scheduler CLI. Use when scheduling jobs, managing cron tasks, running the daemon, or checking job history.
allowed-tools: Bash(scheduler:*)
argument-hint: "[job-id]"
keywords: scheduler, cron, jobs, daemon, automation, history
category: productivity
version: "1.1.0"
---

# Scheduler — Cron Job Runner

Manages recurring shell jobs with cron expressions. All state is persisted in SQLite at `~/.alluka/scheduler/scheduler.db`.

## When to Use

- User wants to schedule a recurring shell command or script
- User wants to run the daemon that executes scheduled jobs
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

### Init

Creates the scheduler database and registers any default jobs. The release ships with an empty default-job list — you add your own jobs via `scheduler jobs add`. The nen self-improvement loop registers its own jobs on first run via `shu propose --init`.

```bash
# Initialize the database
scheduler init

# Replace existing jobs with the same name (used after adding default jobs)
scheduler init --force
```

After `scheduler init`, start the daemon to activate the schedule:

```bash
scheduler init
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

## Configuration

Config file: `~/.alluka/scheduler/config` (key=value format)

```
db_path = /Users/<you>/.alluka/scheduler/scheduler.db
log_level = info
shell = /bin/sh
max_concurrent = 4
```

Run `scheduler configure` to create it interactively. All keys are optional — missing keys use the defaults above.

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
{"type":"schedule.completed","job_id":1,"job_name":"daily-backup","command":"tar czf /tmp/backup.tgz ~/docs","duration_ms":4201,"exit_code":0,"ts":"2026-03-25T08:00:04Z"}
{"type":"schedule.failed","job_id":2,"job_name":"health-check","command":"curl -s localhost:8080/health","duration_ms":312,"exit_code":1,"stderr":"connection refused","ts":"2026-03-25T09:00:00Z"}
```

Use `scheduler history` to view this log in a readable tabular format, or tail it directly:

```bash
tail -f ~/.alluka/events/scheduler.jsonl | jq .
```

## Examples

**User**: "schedule a nightly database dump at 2am"
**Action**: `scheduler jobs add --name "nightly-dump" --cron "0 2 * * *" --command "pg_dump mydb > /backups/mydb-$(date +%F).sql"`

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
ln -sf $(pwd)/bin/scheduler ~/bin/scheduler
```
