package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-engage/internal/cmd"
)

const version = "0.1.0"

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	args := os.Args[1:]

	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}
	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("engage version %s\n", version)
		return nil
	}

	subcommand := args[0]
	remainingArgs := args[1:]

	// Extract global --json flag before passing to subcommands.
	jsonOutput := false
	var filteredArgs []string
	for _, arg := range remainingArgs {
		if arg == "--json" {
			jsonOutput = true
		} else {
			filteredArgs = append(filteredArgs, arg)
		}
	}

	switch subcommand {
	case "scan":
		return cmd.ScanCmd(filteredArgs, jsonOutput)
	case "draft":
		return cmd.DraftCmd(filteredArgs, jsonOutput)
	case "adapt":
		return cmd.AdaptCmd(filteredArgs, jsonOutput)
	case "review":
		return cmd.ReviewCmd(filteredArgs, jsonOutput)
	case "approve":
		return cmd.ApproveCmd(filteredArgs, jsonOutput)
	case "reject":
		return cmd.RejectCmd(filteredArgs, jsonOutput)
	case "post":
		return cmd.PostCmd(filteredArgs, jsonOutput)
	case "commit":
		return cmd.PostScheduledCmd(filteredArgs, jsonOutput)
	case "history":
		return cmd.HistoryCmd(filteredArgs, jsonOutput)
	case "doctor":
		return cmd.DoctorCmd(filteredArgs, jsonOutput)
	case "query":
		return cmd.QueryCmd(filteredArgs, jsonOutput)
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'engage --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Print(`engage - Cross-platform engagement enrichment and drafting

Usage:
  engage <command> [options]

Commands:
  scan                           Scan all platforms for engagement opportunities
  scan --platform youtube,reddit Scan specific platforms only
  scan --limit 10                Limit results per platform
  scan --json                    Structured JSON output (includes full body, comments, transcript)

  draft                          Generate drafts for top opportunities
  draft --platform linkedin      Draft for specific platform only
  draft --limit 5                Max opportunities per platform
  draft --persona default        Persona voice file to use
  draft --skip-authenticity-pass Skip second authenticity rewrite pass
  draft --reschedule-post        Draft + schedule first commit run for tomorrow

  adapt <source>                 Adapt content for different platforms
  adapt file.txt --platforms x,linkedin --persona joey
  adapt https://example.com --platforms youtube
  adapt --platforms linkedin,reddit --dry-run

  review                         List pending drafts with context preview
  review --state approved        Filter by state (pending/approved/rejected/posted/all)

  approve <draft-id>             Approve a pending draft
  reject <draft-id>              Reject a pending draft
  reject <draft-id> --note "x"   Reject with a reason

  post                           Post all approved drafts via platform CLIs
  post <draft-id>                Post a single approved draft
  post --dry-run                 Preview what would be posted without sending

  commit                         Post up to N oldest approved drafts (default 3)
  commit --count 5               Post up to 5 approved drafts
  commit --reschedule            Reschedule next run if approved drafts remain

  history                        Show all posted engagements
  history --since 7d             Show engagements from the last N days/hours

  doctor                         Check all required platform CLIs are available

Options:
  --json       Output in JSON format (works with scan)
  --version    Show version
  --help       Show this help

Storage:
  Queue:   ~/.alluka/engage/queue/<id>.json
  History: ~/.alluka/engage/history/<id>.json
  ALLUKA_HOME overrides the ~/.alluka base directory.
  ENGAGE_PERSONAS_DIR overrides the ~/nanika/personas persona directory.

Examples:
  engage doctor
  engage scan --platform youtube,reddit --json
  engage draft
  engage draft --platform linkedin --limit 3
  engage adapt article.md --platforms linkedin,substack
  engage adapt https://example.com/post --platforms x,reddit
  engage review
  engage review --state all
  engage approve linkedin-abc123-20260325-120000
  engage reject linkedin-abc123-20260325-120000 --note "too generic"
  engage post
  engage post linkedin-abc123-20260325-120000
  engage post --dry-run
  engage history
  engage history --since 7d
`)
}
