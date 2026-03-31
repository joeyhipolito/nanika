package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-reddit/internal/cmd"
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

	// Handle help/version (no subcommand)
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}
	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("reddit version %s\n", version)
		return nil
	}

	// Parse subcommand
	subcommand := args[0]
	remainingArgs := args[1:]

	// Extract global --json flag
	jsonOutput := false
	var filteredArgs []string
	for _, arg := range remainingArgs {
		if arg == "--json" {
			jsonOutput = true
		} else {
			filteredArgs = append(filteredArgs, arg)
		}
	}

	// Tier 1: Commands that don't require configuration
	switch subcommand {
	case "configure":
		return cmd.ConfigureCmd(filteredArgs, jsonOutput)
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	}

	// Tier 2: Commands that require configuration
	switch subcommand {
	case "post":
		return cmd.PostCmd(filteredArgs, jsonOutput)
	case "posts":
		return cmd.PostsCmd(filteredArgs, jsonOutput)
	case "feed":
		return cmd.FeedCmd(filteredArgs, jsonOutput)
	case "comments":
		return cmd.CommentsCmd(filteredArgs, jsonOutput)
	case "comment":
		return cmd.CommentCmd(filteredArgs, jsonOutput)
	case "vote":
		return cmd.VoteCmd(filteredArgs, jsonOutput)
	case "search":
		return cmd.SearchCmd(filteredArgs, jsonOutput)
	case "query":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("usage: reddit query <status|items|actions>")
		}
		return cmd.QueryCmd(filteredArgs[0], jsonOutput)
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'reddit --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Print(`reddit - Interact with Reddit from the terminal

Usage:
  reddit <command> [options]

Commands:
  configure cookies      Extract session cookies from browser
  configure show         Show current configuration (masked secrets)
  doctor                 Verify configuration and authentication

  post --subreddit <sub> --title "Title" "body"     Submit a text post
  post --subreddit <sub> --title "Title" --url <url> Submit a link post
  posts                  List your recent posts
  posts --limit 5        Limit number of posts returned

  feed                   Show your home feed
  feed --subreddit golang  Show subreddit feed
  feed --sort new        Sort: hot/new/top/rising
  feed --limit 20        Limit feed items

  search <query>         Search Reddit posts globally
  search <query> --subreddit golang  Search within a subreddit
  search <query> --sort relevance|new|top|comments
  search <query> --time hour|day|week|month|year|all
  search <query> --limit N

  comments <post-id>     Read comment tree for a post
  comment <id> "text"    Reply to a post or comment
  vote <id>              Upvote a post or comment
  vote <id> --down       Downvote
  vote <id> --unvote     Remove vote

Options:
  --json       Output in JSON format (works with any command)
  --version    Show version
  --help       Show this help

Examples:
  reddit configure cookies
  reddit configure cookies --from-browser firefox
  reddit doctor
  reddit feed --limit 10
  reddit feed --subreddit golang --sort new
  reddit feed --json
  reddit posts --limit 5
  reddit post --subreddit test --title "Hello" "Hello from reddit CLI!"
  reddit post --subreddit golang --title "Check this out" --url https://example.com
  reddit search "golang generics"
  reddit search "error handling" --subreddit golang --sort top --time year
  reddit search "goroutines" --limit 5 --json
  reddit comments abc123
  reddit comment t3_abc123 "Great post!"
  reddit comment t1_xyz789 "I agree"
  reddit vote t3_abc123
  reddit vote t1_xyz789 --down
`)
}
