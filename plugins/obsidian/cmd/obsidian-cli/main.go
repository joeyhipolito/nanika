// Package main implements the obsidian binary.
package main

import (
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-obsidian/internal/cmd"
	"github.com/joeyhipolito/nanika-obsidian/internal/config"
)

const version = "0.2.0"

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	// Parse command line arguments
	args := os.Args[1:]

	// Handle help and version flags
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}

	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("obsidian version %s\n", version)
		return nil
	}

	// Parse subcommand
	subcommand := args[0]
	remainingArgs := args[1:]

	// Extract --json from anywhere in args
	jsonOutput := false
	var filteredArgs []string
	for _, arg := range remainingArgs {
		if arg == "--json" {
			jsonOutput = true
		} else {
			filteredArgs = append(filteredArgs, arg)
		}
	}

	// Extract additional flags that are shared across commands.
	// Command-specific flags (--section, --title, etc.) are parsed in their handlers.
	dryRun := false
	forceFlag := false
	applyFlag := false
	fixFlag := false
	staleDays := 30
	sourceFlag := ""
	ingestTopic := ""
	ingestDomain := ""
	ingestSince := ""
	var cleanedArgs []string
	for i := 0; i < len(filteredArgs); i++ {
		switch filteredArgs[i] {
		case "--dry-run":
			dryRun = true
		case "--force":
			forceFlag = true
		case "--apply":
			applyFlag = true
		case "--fix":
			fixFlag = true
		case "--stale-days":
			if i+1 < len(filteredArgs) {
				if n, err := parseInt(filteredArgs[i+1]); err == nil {
					staleDays = n
				}
				i++
			}
		case "--source":
			if i+1 < len(filteredArgs) {
				sourceFlag = filteredArgs[i+1]
				i++
			}
		case "--topic":
			if i+1 < len(filteredArgs) {
				ingestTopic = filteredArgs[i+1]
				i++
			}
		case "--domain":
			if i+1 < len(filteredArgs) {
				ingestDomain = filteredArgs[i+1]
				i++
			}
		case "--since":
			if i+1 < len(filteredArgs) {
				ingestSince = filteredArgs[i+1]
				i++
			}
		default:
			cleanedArgs = append(cleanedArgs, filteredArgs[i])
		}
	}
	filteredArgs = cleanedArgs

	// Commands that don't require config
	switch subcommand {
	case "configure":
		if len(filteredArgs) > 0 && filteredArgs[0] == "show" {
			return cmd.ConfigureShowCmd(jsonOutput)
		}
		return cmd.ConfigureCmd()
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	case "read", "append", "capture", "create", "list", "search", "index", "sync", "enrich", "maintain", "ingest", "triage", "resurface", "auto-capture", "promote", "cluster", "canonicalize", "health", "query":
		// handled below after vault resolution
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'obsidian --help' for usage", subcommand)
	}

	// Resolve vault path
	vaultPath := config.ResolveVaultPath()
	if vaultPath == "" {
		return fmt.Errorf("no vault path configured\n\nRun 'obsidian configure' to set up, or set OBSIDIAN_VAULT_PATH")
	}

	// Dispatch to appropriate command handler
	switch subcommand {
	case "read":
		if len(filteredArgs) < 1 {
			return fmt.Errorf("read requires a note path\n\nUsage: obsidian read <path>")
		}
		return cmd.ReadCmd(vaultPath, filteredArgs[0], jsonOutput)

	case "append":
		return handleAppendCommand(vaultPath, filteredArgs, jsonOutput)

	case "capture":
		return handleCaptureCommand(vaultPath, filteredArgs, sourceFlag, jsonOutput)

	case "create":
		return handleCreateCommand(vaultPath, filteredArgs, jsonOutput)

	case "list":
		dir := ""
		if len(filteredArgs) > 0 {
			dir = filteredArgs[0]
		}
		return cmd.ListCmd(vaultPath, dir, jsonOutput)

	case "search":
		return handleSearchCommand(vaultPath, filteredArgs, jsonOutput)

	case "index":
		return cmd.IndexCmd(vaultPath, jsonOutput)

	case "sync":
		websitePath := config.ResolveWebsitePath()
		if websitePath == "" {
			return fmt.Errorf("no website path configured\n\nSet website_path in ~/.obsidian/config or OBSIDIAN_WEBSITE_PATH env var")
		}
		websiteURL := config.ResolveWebsiteURL()
		if websiteURL == "" {
			return fmt.Errorf("no website URL configured\n\nSet website_url in ~/.obsidian/config or OBSIDIAN_WEBSITE_URL env var")
		}
		return cmd.SyncCmd(vaultPath, websitePath, websiteURL, dryRun, forceFlag, jsonOutput)

	case "enrich":
		return cmd.EnrichCmd(vaultPath, applyFlag, jsonOutput)

	case "maintain":
		return cmd.MaintainCmd(vaultPath, staleDays, fixFlag, jsonOutput)

	case "ingest":
		return cmd.IngestCmd(vaultPath, cmd.IngestOptions{
			Source:     sourceFlag,
			Topic:      ingestTopic,
			Domain:     ingestDomain,
			Since:      ingestSince,
			DryRun:     dryRun,
			JSONOutput: jsonOutput,
		})

	case "triage":
		return handleTriageCommand(vaultPath, filteredArgs, dryRun, jsonOutput)

	case "resurface":
		return handleResurfaceCommand(vaultPath, filteredArgs, jsonOutput)

	case "auto-capture":
		return cmd.AutoCaptureCmd(vaultPath, cmd.AutoCaptureOptions{
			Since:      ingestSince,
			DryRun:     dryRun,
			JSONOutput: jsonOutput,
		})

	case "promote":
		return cmd.PromoteCmd(vaultPath, cmd.PromoteOptions{
			DryRun:     dryRun,
			JSONOutput: jsonOutput,
		})

	case "cluster":
		return cmd.ClusterCmd(vaultPath, cmd.ClusterOptions{
			DryRun:     dryRun,
			JSONOutput: jsonOutput,
		})

	case "canonicalize":
		return handleCanonicalizeCommand(vaultPath, filteredArgs, dryRun, jsonOutput)

	case "health":
		return cmd.HealthCmd(vaultPath, jsonOutput)

	case "query":
		sub := "status"
		if len(filteredArgs) > 0 {
			sub = filteredArgs[0]
		}
		return cmd.QueryCmd(vaultPath, sub, jsonOutput)
	}

	return nil
}

// parseInt parses a string to int, returning an error if invalid.
func parseInt(s string) (int, error) {
	n := 0
	for _, c := range s {
		if c < '0' || c > '9' {
			return 0, fmt.Errorf("not a number: %s", s)
		}
		n = n*10 + int(c-'0')
	}
	return n, nil
}

// handleCaptureCommand parses and executes the capture command.
// The body comes from the first positional argument; --source is already
// extracted into sourceFlag by the global flag loop.
func handleCaptureCommand(vaultPath string, args []string, source string, jsonOutput bool) error {
	for _, arg := range args {
		if arg == "--help" || arg == "-h" {
			fmt.Print(`Usage: obsidian capture <body> [--source <url>]
       echo 'text' | obsidian capture

Options:
  --source <url>   URL or origin of the capture
  --json           Output in JSON format
  --help, -h       Show this help
`)
			return nil
		}
	}

	body := ""
	if len(args) > 0 {
		body = strings.Join(args, " ")
	}
	return cmd.CaptureCmd(vaultPath, body, source, jsonOutput)
}

// handleAppendCommand parses and executes the append command.
func handleAppendCommand(vaultPath string, args []string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("append requires a note path\n\nUsage: obsidian append <path> [--section <heading>] <text>\n       echo 'text' | obsidian append <path>")
	}
	notePath := args[0]
	section := ""
	var textParts []string

	remaining := args[1:]
	for i := 0; i < len(remaining); i++ {
		switch remaining[i] {
		case "--section":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--section requires an argument")
			}
			section = remaining[i+1]
			i++
		default:
			textParts = append(textParts, remaining[i])
		}
	}

	text := strings.Join(textParts, " ")
	return cmd.AppendCmd(vaultPath, notePath, text, section, jsonOutput)
}

// handleCreateCommand parses and executes the create command.
func handleCreateCommand(vaultPath string, args []string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("create requires a note path\n\nUsage: obsidian create <path> [flags]")
	}
	notePath := args[0]
	var opts cmd.CreateOptions

	remaining := args[1:]
	for i := 0; i < len(remaining); i++ {
		switch remaining[i] {
		case "--title":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--title requires an argument")
			}
			opts.Title = remaining[i+1]
			i++
		case "--type":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--type requires an argument")
			}
			opts.Type = remaining[i+1]
			i++
		case "--context-set":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--context-set requires an argument")
			}
			opts.ContextSet = remaining[i+1]
			i++
		case "--status":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--status requires an argument")
			}
			opts.Status = remaining[i+1]
			i++
		case "--summary":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--summary requires an argument")
			}
			opts.Summary = remaining[i+1]
			i++
		case "--tags":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--tags requires an argument")
			}
			for _, tag := range strings.Split(remaining[i+1], ",") {
				tag = strings.TrimSpace(tag)
				if tag != "" {
					opts.Tags = append(opts.Tags, tag)
				}
			}
			i++
		case "--template":
			if i+1 >= len(remaining) {
				return fmt.Errorf("--template requires an argument")
			}
			opts.Template = remaining[i+1]
			i++
		default:
			return fmt.Errorf("unknown flag: %s", remaining[i])
		}
	}

	return cmd.CreateCmd(vaultPath, notePath, opts, jsonOutput)
}

// handleTriageCommand parses and executes the triage command.
func handleTriageCommand(vaultPath string, args []string, dryRun, jsonOutput bool) error {
	opts := cmd.TriageOptions{
		DryRun:     dryRun,
		JSONOutput: jsonOutput,
	}

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--list":
			opts.List = true
		case "--auto":
			opts.Auto = true
		case "--older":
			if i+1 >= len(args) {
				return fmt.Errorf("--older requires an argument (e.g. 7d, 24h)")
			}
			opts.Older = args[i+1]
			i++
		case "--quiet":
			opts.Quiet = true
		default:
			return fmt.Errorf("unknown triage flag: %s", args[i])
		}
	}

	return cmd.TriageCmd(vaultPath, opts)
}

// handleResurfaceCommand parses and executes the resurface command.
func handleResurfaceCommand(vaultPath string, args []string, jsonOutput bool) error {
	opts := cmd.ResurfaceOptions{
		JSONOutput: jsonOutput,
	}
	var queryParts []string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--random":
			opts.Random = true
		case "--limit":
			if i+1 >= len(args) {
				return fmt.Errorf("--limit requires an argument")
			}
			n, err := parseInt(args[i+1])
			if err != nil {
				return fmt.Errorf("--limit requires a number")
			}
			opts.Limit = n
			i++
		case "--older":
			if i+1 >= len(args) {
				return fmt.Errorf("--older requires an argument (e.g. 7d, 14d, 24h)")
			}
			opts.OlderThan = args[i+1]
			i++
		default:
			queryParts = append(queryParts, args[i])
		}
	}

	query := strings.Join(queryParts, " ")
	return cmd.ResurfaceCmd(vaultPath, query, opts)
}

// handleCanonicalizeCommand parses and executes the canonicalize command.
func handleCanonicalizeCommand(vaultPath string, args []string, dryRun, jsonOutput bool) error {
	opts := cmd.CanonicalizeOptions{
		DryRun:     dryRun,
		JSONOutput: jsonOutput,
		Mode:       "batch",
	}

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--mode":
			if i+1 >= len(args) {
				return fmt.Errorf("--mode requires an argument (batch or per-capture)")
			}
			opts.Mode = args[i+1]
			i++
		case "--capture":
			if i+1 >= len(args) {
				return fmt.Errorf("--capture requires a vault-relative note path")
			}
			opts.CapturePath = args[i+1]
			i++
		default:
			return fmt.Errorf("unknown canonicalize flag: %s", args[i])
		}
	}

	return cmd.CanonicalizeCmd(vaultPath, opts)
}

// handleSearchCommand parses and executes the search command.
func handleSearchCommand(vaultPath string, args []string, jsonOutput bool) error {
	mode := ""
	var queryParts []string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--mode":
			if i+1 >= len(args) {
				return fmt.Errorf("--mode requires an argument (keyword, semantic, or hybrid)")
			}
			mode = args[i+1]
			i++
		default:
			queryParts = append(queryParts, args[i])
		}
	}

	if len(queryParts) == 0 {
		return fmt.Errorf("search requires a query\n\nUsage: obsidian search <query> [--mode keyword|semantic|hybrid]")
	}

	query := strings.Join(queryParts, " ")
	return cmd.SearchCmd(vaultPath, query, mode, jsonOutput)
}

func printUsage() {
	fmt.Printf(`obsidian - Obsidian vault CLI tool (v%s)

USAGE:
    obsidian <command> [options]

COMMANDS:
    read <path>             Read a note's content
    append <path> <text>    Append text to a note
                            --section <heading>  Append inside a named section
    capture <body>          Create a fleeting note in Inbox/
                            --source <url>       URL or origin of the capture
    create <path>           Create a new note
                            --title <title>      Note title (also adds H1 heading)
                            --type <type>        Frontmatter type field
                            --context-set <name> Frontmatter context-set field
                            --status <status>    Frontmatter status field
                            --summary <text>     Frontmatter summary field
                            --tags <t1,t2,...>   Comma-separated tags
                            --template <path>    Vault note to use as body template
    list [dir]              List notes in vault or directory
    search <query>          Search notes (keyword + semantic)
                            --mode keyword|semantic|hybrid (default: hybrid)
    index                   Build/update the search index
    sync                    Sync website content metadata into vault
                            --dry-run  Preview without writing
                            --force    Overwrite unchanged + include unpublished
    enrich                  Suggest links, tags, detect orphan notes
                            --apply    Write suggested links to notes
    maintain                Vault health checks and reporting
                            --stale-days N  Days before note is stale (default: 30)
                            --fix           Add frontmatter to notes missing it
    ingest                  Import data from external sources into vault
                            --source scout|learnings  (required)
                            --topic <name>            Filter scout by topic
                            --domain <name>           Filter learnings by domain
                            --since <duration>        e.g. 7d, 24h, 2w
                            --dry-run                 Preview without writing
    triage                  Review and process notes in Inbox/
                            --list           Show pending notes with age (default)
                            --auto           Classify, enrich, and move each note
                            --older <dur>    Filter to notes older than duration (e.g. 7d, 24h)
                            --dry-run        Preview --auto without writing
                            --quiet          Suppress output when inbox is clear (cron-friendly)
    resurface <query>       Surface old notes relevant to a query
                            --limit N        Max results (default 5)
                            --older <dur>    Only notes older than duration (default 7d)
                            --random         Surface random old notes for serendipitous rediscovery
    auto-capture            Capture learnings, workspace artifacts, and scout intel into vault
                            --since <duration>   Limit lookback window (e.g. 7d, 24h, 2w)
                            --dry-run            Preview what would be captured
    promote                 Detect clusters of related notes and merge into canonical notes
                            --dry-run            Preview clusters without modifying anything
                            --json               Machine-readable cluster output
    cluster                 Find 3+ untagged captures about unnamed concepts via embedding similarity
                            --dry-run            Preview clusters without storing to DB
                            --json               Machine-readable cluster output
    canonicalize            Create canonical notes from detected clusters with LLM-synthesized summaries
                            --mode batch         Process all stored clusters (default)
                            --mode per-capture   Process clusters that include a specific capture
                            --capture <path>     Vault-relative path to capture (required for per-capture)
                            --dry-run            Preview without writing
                            --json               Machine-readable output
    health                  Report vault diagnostics (inbox depth, orphans, stale captures,
                            classification distribution, link density)
                            --json               Machine-readable output
    configure               Set up API key and vault path
    configure show          Show current configuration
    doctor                  Validate installation and configuration

GLOBAL OPTIONS:
    --json              Output in JSON format
    --help, -h          Show this help
    --version, -v       Show version

CONFIGURATION:
    obsidian configure              Interactive setup
    obsidian configure show         Show current config (key masked)
    obsidian doctor                 Validate setup and troubleshoot
    Config file: ~/.obsidian/config

EXAMPLES:
    obsidian configure                              # First-time setup
    obsidian read daily/2026-02-07.md               # Read a note
    obsidian append daily/2026-02-07.md "New task"  # Append to note
    obsidian append daily/2026-02-07.md --section "## Tasks" "- buy milk"
    obsidian capture "rough idea about search"      # Quick fleeting note
    obsidian capture "link worth reading" --source https://example.com
    echo "piped text" | obsidian capture            # Capture from stdin
    obsidian create projects/new-idea.md --title "New Idea" --type idea
    obsidian create projects/new-idea.md --tags "go,cli" --status draft
    obsidian create projects/new-idea.md --template "99 Templates/idea.md"
    obsidian list daily/                            # List notes in folder
    obsidian search "project ideas"                 # Hybrid search (default)
    obsidian search "golang" --mode keyword         # Keyword-only search
    obsidian index                                  # Build search index
    obsidian sync                                   # Sync website to vault
    obsidian sync --dry-run                         # Preview sync changes
    obsidian enrich                                 # Find note connections
    obsidian enrich --apply                         # Apply suggested links
    obsidian maintain                               # Vault health report
    obsidian ingest --source scout                  # Import scout intel
    obsidian ingest --source scout --topic "ai-models" --since 7d
    obsidian ingest --source learnings              # Import orchestrator learnings
    obsidian ingest --source learnings --domain dev --since 30d
    obsidian ingest --source scout --dry-run        # Preview what would be created
    obsidian triage                                 # List pending inbox notes
    obsidian triage --older 7d                      # Only notes older than 7 days
    obsidian triage --auto                          # Classify and move inbox notes
    obsidian triage --auto --dry-run                # Preview triage without writing
    obsidian triage --auto --json                   # Structured output
    obsidian triage --auto --quiet                  # Cron-friendly: no output when inbox is clear
    obsidian resurface "golang patterns"            # Find old notes about golang patterns
    obsidian resurface "golang patterns" --older 14d --limit 3
    obsidian resurface --random                     # Random old note for serendipitous rediscovery
    obsidian resurface --random --older 30d --json  # Random old notes as JSON
    obsidian auto-capture                           # Capture all via workflow outputs
    obsidian auto-capture --since 7d               # Only items from the last 7 days
    obsidian auto-capture --dry-run                 # Preview what would be captured
    obsidian auto-capture --json                    # Machine-readable output
    obsidian promote                                # Detect clusters, interactively promote
    obsidian promote --dry-run                      # Preview clusters without writing
    obsidian promote --json                         # Machine-readable cluster output
    obsidian cluster                                # Detect unnamed concept clusters in captures
    obsidian cluster --dry-run                      # Preview without storing results
    obsidian cluster --json                         # Machine-readable cluster output
    obsidian canonicalize                           # Create canonical notes from all stored clusters
    obsidian canonicalize --dry-run                 # Preview what would be created
    obsidian canonicalize --json                    # Machine-readable output
    obsidian canonicalize --mode per-capture --capture Inbox/20260319-120000.md
    obsidian health                                 # Vault health report
    obsidian health --json                          # Machine-readable health report
    obsidian doctor                                 # Check setup

CRON SETUP (run triage hourly, only emails on activity):
    # crontab -e
    0 * * * * /usr/local/bin/obsidian triage --auto --quiet 2>&1

For more information, visit: https://obsidian.md
`, version)
}
