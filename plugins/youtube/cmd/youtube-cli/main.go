package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-youtube/internal/cmd"
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
		fmt.Printf("youtube version %s\n", version)
		return nil
	}

	subcommand := args[0]
	remainingArgs := args[1:]

	// Extract global --json flag.
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
	case "configure":
		return cmd.ConfigureCmd(filteredArgs, jsonOutput)
	case "auth":
		return cmd.AuthCmd(filteredArgs)
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	case "scan":
		return cmd.ScanCmd(filteredArgs, jsonOutput)
	case "comment":
		return cmd.CommentCmd(filteredArgs, jsonOutput)
	case "like":
		return cmd.LikeCmd(filteredArgs, jsonOutput)
	case "query":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("usage: youtube query <status|items|actions>")
		}
		return cmd.QueryCmd(filteredArgs[0], jsonOutput)
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'youtube --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Print(`youtube - Scan channels, comment, and like YouTube videos from the terminal

Usage:
  youtube <command> [options]

Commands:
  configure              Set up config interactively
  configure show         Show current configuration (masked secrets)
  auth                   Set up OAuth for posting (opens browser)
  auth --code <code>     Exchange authorization code for tokens
  doctor                 Verify configuration and quota status

  scan                   Scan configured channels for recent videos
  scan --topics "go cli,platform engineering"  Search by topic
  scan --limit 10        Limit results
  scan --since 7d        Only return videos newer than 7 days
  scan --json            Structured JSON output

  comment <video-id> <text>  Post a top-level comment (50 quota units)
  like <video-id>            Like a video (50 quota units)

Options:
  --json       Output in JSON format (works with any command)
  --version    Show version
  --help       Show this help

Quota costs (YouTube Data API v3):
  scan (per channel or topic): 100 units
  comment:                      50 units
  like:                         50 units
  Default budget:            10000 units/day

Config: ~/.alluka/youtube-config.json
Token:  ~/.alluka/youtube-oauth.json

Examples:
  youtube configure
  youtube auth
  youtube doctor
  youtube scan
  youtube scan --topics "golang,cli tools" --json
  youtube scan --limit 5 --since 48h
  youtube comment dQw4w9WgXcQ "Great video!"
  youtube like dQw4w9WgXcQ
  youtube doctor --json
`)
}
