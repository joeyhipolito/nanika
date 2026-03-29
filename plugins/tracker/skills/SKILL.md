---
name: tracker
description: Local issue tracker with hierarchical relationships, blocking links, and priority-based ready detection. Use when managing project tasks, creating actionable work items, or visualizing issue dependencies.
allowed-tools: Bash(tracker:*)
argument-hint: "[command] [arguments]"
---

# Tracker — Local Issue Management System

Personal issue tracker with parent–child relationships, blocking link detection, and ready-issue calculation. No cloud, no auth, pure local SQLite. Perfect for solo projects, side quests, and fine-grained task breakdowns.

## When to Use

- Managing project tasks and milestones
- Creating actionable work items with dependencies
- Visualizing task hierarchies and blocking relationships
- Tracking priority-based work queues
- Recording issues with labels and assignees
- Finding the next task to work on (ready issues)

## Commands

```bash
tracker create "Task title"                                  # Create a new issue with auto-generated ID
tracker create "Task" --priority P0 --description "Details"  # Create with priority and description
tracker create "Subtask" --parent trk-ABC1                  # Create a child issue (subtask)
tracker create "Task" --labels "backend,urgent" --assignee me # Create with labels and assignee

tracker show trk-ABC1                                        # Show a single issue with details
tracker list                                                 # List all issues in table format
tracker list --status open                                  # Filter by status (open, in-progress, done, cancelled)
tracker list --priority P0                                  # Filter by priority (P0, P1, P2, P3)

tracker update trk-ABC1 --status in-progress                # Update status
tracker update trk-ABC1 --priority P0 --assignee alice      # Update priority and assignee
tracker delete trk-ABC1                                     # Delete an issue

tracker link trk-ABC1 trk-XYZ2 --type blocks               # Link two issues (blocks, relates_to, supersedes, duplicates)
tracker link trk-ABC1 trk-XYZ2 --type relates_to            # Create a relation link
tracker unlink trk-ABC1 trk-XYZ2 --type blocks             # Remove a link

tracker ready                                                # List open issues with no blocking links
tracker next                                                 # Show the highest-priority ready issue
tracker tree                                                 # Show issues as a parent–child hierarchy
tracker comment trk-ABC1 "Fix widget alignment" --author me # Add a comment to an issue
tracker search "widget"                                     # Search issues by title or description

tracker query status --json                                 # Plugin: get tracker status
tracker query items --json                                  # Plugin: list all issues as JSON
tracker query actions --json                                # Plugin: list available actions
```

## Configuration

Database file location: `~/.tracker/tracker.db`

Override with environment variable: `TRACKER_DB=/custom/path/tracker.db tracker list`

Or command-line flag: `tracker --db /custom/path/tracker.db list`

## Examples

### User: Create a new feature task

**Action:**
```bash
tracker create "Implement user authentication"
tracker create "Add OAuth provider" --parent trk-ABC1 --priority P0
```

---

### User: Find what to work on next

**Action:**
```bash
tracker next
```

Returns the highest-priority issue that has no blocking dependencies.

---

### User: Track a bug with dependencies

**Action:**
```bash
tracker create "Database query slow" --priority P1 --description "Users table scan takes 5s"
tracker create "Add index on user_id" --parent trk-DEF2 --priority P1
tracker link trk-DEF2 trk-ABC1 --type blocks  # DEF2 blocks ABC1
tracker ready  # ABC1 won't show until DEF2 is done
```

---

### User: View task hierarchy

**Action:**
```bash
tracker tree
```

Shows all issues nested under their parents with indentation.

---

## Status Values

- **open** — Not started, available for work
- **in-progress** — Currently being worked on
- **done** — Completed
- **cancelled** — No longer relevant

## Priority Values

- **P0** — Critical, blockers, must-fix
- **P1** — High priority, should-fix soon
- **P2** — Medium priority, nice-to-have
- **P3** — Low priority, backlog

## Link Types

- **blocks** — A–[blocks]–>B means A must be done before B can be done
- **relates_to** — A–[relates_to]–>B means A is related to B
- **supersedes** — A–[supersedes]–>B means A replaces or obsoletes B
- **duplicates** — A–[duplicates]–>B means A is a duplicate of B

## Ready Issues

An issue is "ready" if:
- Its status is "open"
- All parent–child relationships show it is not blocked by ancestors
- No other issue with a "blocks" link has it as the target

Use `tracker ready` to find issues you can start right now.

---

## Database Schema

```sql
-- Issues table
CREATE TABLE issues (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  description TEXT,
  status TEXT DEFAULT 'open',
  priority TEXT,
  labels TEXT,  -- Comma-separated
  assignee TEXT,
  parent_id TEXT,  -- For subtasks
  created_at TEXT,
  updated_at TEXT
);

-- Links table
CREATE TABLE links (
  id INTEGER PRIMARY KEY,
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  link_type TEXT NOT NULL,
  created_at TEXT
);

-- Comments table
CREATE TABLE comments (
  id INTEGER PRIMARY KEY,
  issue_id TEXT NOT NULL,
  body TEXT NOT NULL,
  author TEXT,
  created_at TEXT
);
```

---

*Tracker 0.1.0 — Local-first, zero-dependency issue tracking.*
