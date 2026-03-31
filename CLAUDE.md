# Nanika

**Self-improving multi-agent orchestrator for Claude Code.** One sentence in. A team of specialists out.

## Skills

Skills live under `skills/` as Go modules. Skill definitions auto-discover from `.claude/skills/`.

## Structure

```
nanika/
├── .claude/skills/          # Skill definitions (auto-discovered)
│   ├── orchestrator/        # Mission execution skill
│   └── decomposer/          # Mission decomposition skill
├── skills/                  # Skill source code (Go modules)
│   ├── orchestrator/        # Multi-agent mission orchestrator
│   └── decomposer/          # PHASE line format (knowledge-only, no binary)
├── personas/                # Persona markdown files
├── scripts/                 # generate-agents-md.sh, new-mission.sh
└── docs/                    # Skill standard, persona standard
```

## Docs

- `docs/SKILL-STANDARD.md` — Skill conventions
- `docs/PERSONA-STANDARD.md` — Persona conventions
- `.claude/skills/*/SKILL.md` — Individual skill references

## Backlog And Mission Rules

- Linear team `V` / `nanika` is the canonical backlog for status, priority, and execution queue
- runtime mission files live under `~/.alluka/missions/`
- use `~/nanika/scripts/new-mission.sh <slug>` when creating a new mission
- use the `linear` CLI for routine backlog updates

### Writing Mission Files

Always use pre-decomposed PHASE lines to get deterministic execution. Without PHASE lines, the orchestrator's LLM decomposer decides the plan — and it makes mistakes (e.g., injecting code review phases into research missions, wrong dependency ordering).

Reference the decomposer skill for the full format: `.claude/skills/decomposer/SKILL.md`

Quick reference:
```
PHASE: <name> | OBJECTIVE: <concrete deliverable> | PERSONA: <persona> | SKILLS: <skill1,skill2> | DEPENDS: <phase1,phase2>
```

### Before Running

Always dry-run first:
```bash
orchestrator run <mission-file> --dry-run --verbose
```

Verify:
- No unexpected review phases on research/non-code missions (use `--no-review` if auto-injected)
- Dependencies are correct (phases that need prior output have DEPENDS)
- Persona assignments match the work type

### Research Missions

Research missions that produce no code should use `--no-review`. The auto-injected review phase assumes code output and will review the entire repo instead of the research findings.

```bash
orchestrator run <mission-file> --no-git --no-review
```

### Code Missions

Code missions benefit from the review phase. Let it auto-inject or add explicitly:
```
PHASE: implement | OBJECTIVE: ... | PERSONA: senior-backend-engineer
PHASE: review | OBJECTIVE: Review the implementation for correctness and test coverage | PERSONA: staff-code-reviewer | DEPENDS: implement
```

<!-- NANIKA-AGENTS-MD-START -->
[Nanika Skills Index][root: .claude/skills]IMPORTANT: Prefer retrieval-led reasoning over pre-training-led reasoning. Read skill files before making assumptions.

|agent-browser — Browser automation CLI for AI agents:{.claude/skills/agent-browser/SKILL.md}|`agent-browser open https://example.com/form`|`agent-browser snapshot -i`|`agent-browser fill @e1 "user@example.com"`|`agent-browser fill @e2 "password123"`|`agent-browser click @e3`|`agent-browser wait --load networkidle`|`agent-browser snapshot -i`|`agent-browser open https://example.com && agent-browser wait --load networkidle && agent-browser snapshot -i`|`agent-browser fill @e1 "user@example.com" && agent-browser fill @e2 "password123" && agent-browser click @e3`|`agent-browser open https://example.com && agent-browser wait --load networkidle && agent-browser screenshot page.png`|`agent-browser --auto-connect state save ./auth.json`|`agent-browser --state ./auth.json open https://app.example.com/dashboard`|`agent-browser --profile ~/.myapp open https://app.example.com/login`|`agent-browser --profile ~/.myapp open https://app.example.com/dashboard`|
|better-auth-best-practices — Configure Better Auth server and client, set up database adapters, manage sessions, add plugins, and handle environment variables:{.claude/skills/better-auth-best-practices/SKILL.md}|
|building-native-ui — Complete guide for building beautiful apps with Expo Router:{.claude/skills/building-native-ui/SKILL.md}|
|channels — Telegram and Discord channel integration for the orchestrator:{.claude/skills/channels/SKILL.md}|
|decomposer — Mission decomposition skill — breaks complex tasks into dependency-aware PHASE lines that the orchestrator executes directly, bypassing its internal LLM decomposer:{.claude/skills/decomposer/SKILL.md}|
|orchestrator — Executes tasks and missions via orchestrator CLI:{.claude/skills/orchestrator/SKILL.md}|`orchestrator run "research golang error handling best practices"`|`orchestrator run --domain personal "plan my Japan trip"`|`orchestrator run ~/.alluka/missions/FEATURE.md`|`orchestrator run --dry-run "build authentication system"`|`orchestrator status`|`orchestrator learn`|`orchestrator cleanup`|`orchestrator cleanup --older 7d`|`orchestrator metrics`|`orchestrator metrics --last 10`|`orchestrator metrics --domain dev`|`orchestrator metrics --status failed`|`orchestrator metrics --mission <id>`|`orchestrator metrics --days 30`|
|supabase-postgres-best-practices — Postgres performance optimization and best practices from Supabase:{.claude/skills/supabase-postgres-best-practices/SKILL.md}|
|vercel-react-best-practices — React and Next.js performance optimization guidelines from Vercel Engineering:{.claude/skills/vercel-react-best-practices/SKILL.md}|
|youtube-transcript — Extract transcripts from YouTube videos:{.claude/skills/youtube-transcript/SKILL.md}|
|discord — Send native voice messages and text to Discord channels via the discord CLI:{plugins/discord/skills/SKILL.md}|`discord send-voice-message --channel <channel-id> --audio /path/to/audio.mp3`|`discord send-voice-message --channel <channel-id> --audio /path/to/audio.ogg --json`|`discord reply --channel <channel-id> --message "Hello from nanika"`|`discord reply --channel <channel-id> --message "Mission complete" --json`|`discord query status --json`|`discord query items --json`|`discord query actions --json`|`discord doctor`|`discord doctor --json`|
|elevenlabs — ElevenLabs TTS CLI — generate voiceover audio, format narration scripts, run forced alignment, and produce timing maps from the terminal:{plugins/elevenlabs/skills/SKILL.md}|`elevenlabs configure`|`elevenlabs configure show`|`elevenlabs configure show --json`|`elevenlabs doctor`|`elevenlabs doctor --json`|`elevenlabs voices`|`elevenlabs voices --json`|`elevenlabs format narration-script.md`|`elevenlabs format narration-script.md --output narration-elevenlabs.txt`|`elevenlabs generate narration-elevenlabs.txt`|`elevenlabs generate narration-elevenlabs.txt --voice pNInz6obpgDQGcFmaJgB`|`elevenlabs generate narration-elevenlabs.txt --output ./output/`|`elevenlabs generate narration-elevenlabs.txt --seed 42 --speed 1.1`|`elevenlabs generate narration-elevenlabs.txt --format opus_48000_32`|
|engage — Cross-platform comment engagement CLI — scan, draft, review, approve, and post comments across YouTube, LinkedIn, Reddit, and Substack:{plugins/engage/skills/SKILL.md}|`engage doctor                                        Check all required CLIs are installed`|`engage scan                                          Scan all platforms for opportunities`|`engage draft                                         Draft comments for top opportunities`|`engage draft --reschedule-post                       Draft + schedule first commit run for tomorrow`|`engage adapt <source> --platforms <platforms>       Adapt content for specific platforms`|`engage adapt article.md --platforms linkedin,x      Adapt local file for multiple platforms`|`engage review                                        Review pending drafts`|`engage approve <id>                                  Approve a draft for posting`|`engage reject <id> [--note "reason"]                 Reject a draft`|`engage post                                          Post all approved drafts immediately`|`engage post <id>                                     Post one approved draft`|`engage commit                                        Post up to 3 oldest approved drafts (daemon-safe)`|`engage commit --count 5 --reschedule                 Post up to 5 and reschedule next run if drafts remain`|`engage history                                       Show posting history`|
|example-bookmarks — SQLite bookmark manager plugin — add, list, search, and delete bookmarks with full dashboard integration:{plugins/example-bookmarks/skills/SKILL.md}|
|example-hello — Minimal example plugin demonstrating the nanika plugin protocol — greet subcommand with persistent state and full dashboard query protocol:{plugins/example-hello/skills/SKILL.md}|
|gmail — Reads inbox, triages threads, applies labels, organizes email, and composes/sends email across multiple Gmail accounts via gmail CLI:{plugins/gmail/skills/SKILL.md}|`gmail configure work`|`gmail configure personal`|`gmail configure show`|`gmail configure show --json`|`gmail accounts`|`gmail accounts --json`|`gmail accounts remove work`|`gmail doctor`|`gmail doctor --json`|`gmail inbox`|`gmail inbox --unread`|`gmail inbox --limit 50`|`gmail inbox --account work`|`gmail inbox --account work --unread --json`|
|linkedin — LinkedIn CLI — publish posts, read feed, comment, react, and automate engagement from the terminal:{plugins/linkedin/skills/SKILL.md}|`linkedin configure`|`linkedin configure show`|`linkedin configure chrome`|`linkedin doctor`|`linkedin doctor --json`|`linkedin chrome`|`linkedin chrome --launch`|`linkedin post "Hello LinkedIn!"`|`linkedin post "Check this out" --image photo.jpg`|`linkedin post --file article.mdx`|`linkedin post --file article.mdx --image cover.jpg`|`linkedin post "Draft post" --visibility CONNECTIONS`|`linkedin post "Hello" --json`|`linkedin posts`|
|obsidian — Obsidian vault CLI — read, write, search, capture, triage, enrich, and ingest into your Obsidian vault from the terminal:{plugins/obsidian/skills/SKILL.md}|`obsidian read daily/2026-03-25.md`|`obsidian read daily/2026-03-25.md --json`|`obsidian append daily/2026-03-25.md "New task"`|`obsidian append daily/2026-03-25.md --section "## Tasks" "- buy milk"`|`obsidian capture "rough idea about search"`|`obsidian capture "link worth reading" --source https://example.com`|`obsidian capture "draft" --json`|`obsidian create projects/new-idea.md --title "New Idea" --type idea`|`obsidian create projects/new-idea.md --tags "go,cli" --status draft`|`obsidian create projects/new-idea.md --summary "Short description"`|`obsidian create projects/new-idea.md --context-set "work"`|`obsidian create projects/new-idea.md --template "99 Templates/idea.md"`|`obsidian list`|`obsidian list daily/`|
|reddit — Reddit CLI — submit posts, read feeds, comment, vote, and search from the terminal using browser cookies:{plugins/reddit/skills/SKILL.md}|`reddit configure cookies`|`reddit configure cookies --from-browser firefox`|`reddit configure show`|`reddit configure show --json`|`reddit doctor`|`reddit doctor --json`|`reddit post --subreddit golang --title "Title" "body text"`|`reddit post --subreddit golang --title "Title" --url https://example.com`|`reddit posts`|`reddit posts --limit 25`|`reddit posts --json`|`reddit feed`|`reddit feed --subreddit golang`|`reddit feed --sort new`|
|scheduler — Schedules and runs cron jobs and the nanika publishing pipeline via scheduler CLI:{plugins/scheduler/skills/SKILL.md}|`scheduler doctor`|`scheduler doctor --json`|`scheduler daemon`|`scheduler daemon --notify`|`scheduler daemon --once`|`scheduler daemon --stop`|`scheduler daemon >> ~/.alluka/logs/scheduler.log 2>&1 &`|`scheduler init`|`scheduler init --force`|`scheduler init`|`scheduler daemon`|`scheduler jobs`|`scheduler jobs add --name "daily-backup" --cron "0 2 * * *" --command "tar czf /tmp/backup.tgz ~/docs"`|`scheduler jobs add --name "health-check" --cron "*/5 * * * *" --command "curl -s localhost:8080/health"`|
|scout — Gathers intelligence on configurable topics via scout CLI:{plugins/scout/skills/SKILL.md}|`scout topics`|`scout topics add "my-topic"`|`scout topics add "my-topic" --sources "web,hackernews,devto" --terms "keyword1,keyword2"`|`scout topics add "my-topic" --devto-tags "go,cli" --lobsters-tags "programming"`|`scout topics remove "my-topic"`|`scout topics preset`|`scout topics preset ai-all`|`scout topics preset dev-all`|`scout topics preset all`|`scout topics preset go-development`|`scout gather`|`scout gather "ai-models"`|`scout intel`|`scout intel "ai-models"`|
|substack — Substack CLI — publish posts, manage drafts, post notes, comment, and automate feed engagement from the terminal:{plugins/substack/skills/SKILL.md}|`substack configure`|`substack configure --from-browser chrome`|`substack configure --from-browser firefox`|`substack configure show`|`substack configure show --json`|`substack doctor`|`substack doctor --json`|`substack draft article.mdx`|`substack draft article.mdx --tags "go,cli"`|`substack draft article.mdx --audience everyone`|`substack draft article.mdx --manifest manifest.json --public-dir ./public`|`substack draft article.mdx --json`|`substack drafts`|`substack drafts --limit 10`|
|telegram — Send voice messages and text to Telegram chats via the telegram CLI:{plugins/telegram/skills/SKILL.md}|`telegram send-voice-message --chat <chat-id> --audio /path/to/audio.mp3`|`telegram send-voice-message --chat <chat-id> --audio /path/to/audio.ogg --json`|`telegram reply --chat <chat-id> --message "Hello from nanika"`|`telegram reply --chat <chat-id> --message "Mission complete" --json`|`telegram query status --json`|`telegram query items --json`|`telegram query actions --json`|`telegram doctor`|`telegram doctor --json`|
|tracker — Local issue tracker with hierarchical relationships, blocking links, and priority-based ready detection:{plugins/tracker/skills/SKILL.md}|`tracker create "Task title"`|`tracker create "Task" --priority P0 --description "Details"`|`tracker create "Subtask" --parent trk-ABC1`|`tracker create "Task" --labels "backend,urgent" --assignee me`|`tracker show trk-ABC1`|`tracker list`|`tracker list --status open`|`tracker list --priority P0`|`tracker update trk-ABC1 --status in-progress`|`tracker update trk-ABC1 --priority P0 --assignee alice`|`tracker delete trk-ABC1`|`tracker link trk-ABC1 trk-XYZ2 --type blocks`|`tracker link trk-ABC1 trk-XYZ2 --type relates_to`|`tracker unlink trk-ABC1 trk-XYZ2 --type blocks`|
|ynab — YNAB CLI — manage budgets, transactions, categories, and accounts from the terminal via the YNAB API:{plugins/ynab/skills/SKILL.md}|`ynab status`|`ynab status --json`|`ynab balance`|`ynab balance "Checking"`|`ynab balance --json`|`ynab budget`|`ynab budget --json`|`ynab categories`|`ynab categories --json`|`ynab transactions`|`ynab transactions --since 2026-01-01`|`ynab transactions --account "Checking"`|`ynab transactions --category "Groceries"`|`ynab transactions --payee "Coffee"`|
|youtube — YouTube CLI — scan channels, post comments, like videos, and manage OAuth from the terminal:{plugins/youtube/skills/SKILL.md}|`youtube configure`|`youtube configure show`|`youtube configure show --json`|`youtube auth`|`youtube auth --code <code>`|`youtube doctor`|`youtube doctor --json`|`youtube scan`|`youtube scan --since 7d`|`youtube scan --topics "go cli,platform eng"`|`youtube scan --limit 5`|`youtube scan --json`|`youtube scan --topics "golang" --limit 10 --json`|`youtube comment dQw4w9WgXcQ "Great video!"`|
<!-- NANIKA-AGENTS-MD-END -->
