// Package main implements the scout binary.
package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-scout/internal/cmd"
)

const version = "1.0.0"

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	args := os.Args[1:]

	// Handle help and version flags
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}

	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("scout version %s\n", version)
		return nil
	}

	// Parse subcommand
	subcommand := args[0]
	remainingArgs := args[1:]

	// Check for global --json flag
	jsonOutput := false
	var filteredArgs []string
	for _, arg := range remainingArgs {
		if arg == "--json" {
			jsonOutput = true
		} else {
			filteredArgs = append(filteredArgs, arg)
		}
	}

	// Commands that don't require configuration
	switch subcommand {
	case "configure":
		if len(filteredArgs) > 0 && filteredArgs[0] == "show" {
			return cmd.ConfigureShowCmd(jsonOutput)
		}
		return cmd.ConfigureCmd()
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	}

	// Dispatch to commands
	switch subcommand {
	case "topics":
		return cmd.TopicsCmd(filteredArgs, jsonOutput)
	case "gather":
		return cmd.GatherCmd(filteredArgs, jsonOutput)
	case "intel":
		return cmd.IntelCmd(filteredArgs, jsonOutput)
	case "brief":
		return cmd.BriefCmd(filteredArgs, jsonOutput)
	case "context":
		return cmd.ContextCmd(filteredArgs, jsonOutput)
	case "suggest":
		return cmd.SuggestCmd(filteredArgs, jsonOutput)
	case "discover":
		return cmd.DiscoverCmd(filteredArgs, jsonOutput)
	case "health":
		return cmd.HealthCmd(filteredArgs, jsonOutput)
	case "query":
		return cmd.QueryCmd(filteredArgs, jsonOutput)
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'scout --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Printf(`scout - Intelligence gathering CLI (v%s)

USAGE:
    scout <command> [options]

COMMANDS:
    topics                  List all configured topics
    topics add              Add a new topic
    topics remove           Remove a topic
    topics preset           List/install pre-configured topic presets
    gather                  Gather intel for all topics (parallel)
    gather <topic>          Gather intel for a specific topic
    intel                   List topics with item counts
    intel <topic>           Show latest intel for a topic
    brief <topic>           Generate a synthesized brief
    context <topic>         Export context for downstream tools
    suggest                 Generate content suggestions from intel
    discover                AI-powered topic recommendations
    configure               Interactive setup
    configure show          Show current configuration
    doctor                  Validate installation and configuration
    health                  Show per-source gather health status
    query status            Topic count, total items, last gather, source health
    query items             List topics with item counts
    query action gather     Trigger gather-all and return item count

TOPIC MANAGEMENT:
    scout topics                                      List all topics
    scout topics add "name" [options]                  Create a topic
        --sources "rss,googlenews,github"              Sources to gather from
        --terms "term1,term2"                          Search terms
        --feeds "url1,url2"                            RSS/Atom feed URLs (required for rss source)
        --github-queries "query1,query2"               GitHub search queries
        --reddit-subs "sub1,sub2"                      Reddit subreddits
        --description "desc"                           Topic description
    scout topics remove "name"                         Delete a topic
    scout topics preset                                List available presets
    scout topics preset ai-all                         Install all AI presets
    scout topics preset ai-models                      Install AI models preset

SOURCES:
    rss         RSS/Atom feed parser (requires --feeds URLs)
    github      GitHub repository search (optional --github-queries)
    googlenews  Google News RSS search
    reddit      Reddit search (optional --reddit-subs subreddits)
    hackernews  Hacker News
    substack    Substack publications (optional --substack-pubs)
    medium      Medium tags/publications (optional --medium-tags, --medium-pubs)
    devto       dev.to articles (optional --devto-tags)
    lobsters    Lobste.rs (optional --lobsters-tags)
    youtube     YouTube channels (optional --youtube-channels)
    arxiv       ArXiv papers (optional --arxiv-categories)
    bluesky     Bluesky social network
    x           X/Twitter via Bird CLI (optional, graceful skip)

GATHERING:
    scout gather                                Gather all topics (parallel)
    scout gather "topic"                        Gather specific topic
    scout gather --since "24h"                  Only recent items

BROWSING INTEL:
    scout intel                                 List topics with counts
    scout intel "topic"                         Show latest items
    scout intel "topic" --since "7d"            Filter by date
    scout intel "topic" --format json           JSON output
    scout intel "topic" --format csv            CSV output
    scout intel "topic" --format markdown       Markdown output (default)

BRIEFING:
    scout brief "topic"                         Synthesized brief
    scout brief "topic" --since 7d              Recent items only
    scout brief "topic" --top 20                Top 20 items
    scout brief "topic" --json                  Machine-readable

CONTEXT EXPORT:
    scout context "topic"                       Markdown context
    scout context "topic" --compact             Top 5 only
    scout context "topic" --json                JSON format
    scout context "topic" --file "path"         Write to file

SUGGESTIONS:
    scout suggest                               Suggest content from all topics
    scout suggest --topic "ai-models"           Suggestions for one topic
    scout suggest --since "7d"                  Only recent intel
    scout suggest --type blog                   Filter by type (blog/thread/video)
    scout suggest --limit 10                    Max suggestions (default: 5)
    scout suggest --json                        JSON output

DISCOVERY:
    scout discover                              Analyze intel and recommend topics
    scout discover --since "7d"                 Only recent intel
    scout discover --dry-run                    Preview recommendations
    scout discover --auto                       Apply recommendations
    scout discover --json                       Machine-readable output

HEALTH:
    scout health                                Show per-source health (successes, failures, latency)
    scout health --json                         Machine-readable health data
    scout health --reset <source>               Clear health data for a specific source

GLOBAL OPTIONS:
    --json              Output in JSON format (shorthand for --format json)
    --format            Output format: markdown, json, csv (default: markdown)
    --help, -h          Show this help
    --version, -v       Show version

CONFIGURATION:
    scout configure                 Interactive setup (gather interval)
    scout configure show            Show current config
    scout doctor                    Validate setup
    Config file: ~/.scout/config
    Topics: ~/.scout/topics/{name}.json
    Intel: ~/.scout/intel/{topic}/{date}_{source}.json

EXAMPLES:
    scout configure                                     # First-time setup
    scout topics preset ai-all                          # Install all AI presets
    scout topics add "security" --sources "rss,googlenews" --feeds "https://feeds.feedburner.com/TheHackersNews" --terms "CVE,zero-day,exploit"
    scout topics add "ai-repos" --sources "github" --github-queries "topic:ai,topic:llm"
    scout gather                                        # Gather all topics (parallel)
    scout gather "ai-models"                            # Gather one topic
    scout intel                                         # Overview
    scout intel "ai-models"                             # Browse intel
    scout intel "ai-models" --since "7d" --json         # Recent JSON
    scout suggest                                       # Content ideas from all intel
    scout suggest --topic "ai-models" --type blog       # Blog ideas for one topic
    scout discover --dry-run                            # Preview topic recommendations
    scout discover --auto                               # Apply AI-generated recommendations
    scout doctor                                        # Check setup
`, version)
}
