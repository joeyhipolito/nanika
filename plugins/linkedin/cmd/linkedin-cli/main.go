package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-linkedin/internal/cmd"
)

const version = "0.2.0"

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
		fmt.Printf("linkedin version %s\n", version)
		return nil
	}

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
	case "chrome":
		return cmd.ChromeCmd(filteredArgs)
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
	case "react":
		return cmd.ReactCmd(filteredArgs, jsonOutput)
	case "engage":
		return cmd.EngageCmd(filteredArgs, jsonOutput)
	case "query":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("usage: linkedin query <status|items|actions>")
		}
		return cmd.QueryCmd(filteredArgs[0], jsonOutput)
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'linkedin --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Print(`linkedin - Publish and interact with LinkedIn from the terminal

Usage:
  linkedin <command> [options]

Commands:
  configure              Set up OAuth authentication (opens browser)
  configure chrome       Test Chrome CDP connection
  configure show         Show current configuration (masked secrets)
  doctor                 Verify configuration and authentication

  chrome                 Show Chrome launch command for remote debugging
  chrome --launch        Launch Chrome with remote debugging

  post <text>            Create a text post
  post --file <mdx>      Create a post from an MDX file
  post <text> --image <path>  Create a post with an image
  posts                  List your recent posts
  posts --limit 5        Limit number of posts returned

  feed                   Show your LinkedIn feed (requires Chrome with CDP)
  feed --limit 20        Limit feed items
  comments <urn>         Read comments on a post
  comment <urn> <text>   Comment on a post
  react <urn>            React to a post (default: LIKE)
  react <urn> --type CELEBRATE  React with specific type

  engage                 Automated feed engagement (dry-run by default)
  engage --post          Actually post comments and reactions
  engage --persona <p>   Use persona voice file
  engage --posts-file <p> Substack posts JSON for article grounding

Options:
  --json       Output in JSON format (works with any command)
  --version    Show version
  --help       Show this help

Prerequisites:
  For posting/commenting/reacting: OAuth setup via 'linkedin configure'
  For feed reading: Chrome with remote debugging on port 9222
    linkedin chrome --launch

Examples:
  linkedin configure
  linkedin chrome --launch
  linkedin post "Hello LinkedIn!"
  linkedin post "Check this out" --image photo.jpg
  linkedin posts --limit 5 --json
  linkedin feed --limit 10
  linkedin feed --json
  linkedin comments urn:li:activity:1234567890
  linkedin comment 1234567890 "Great post!"
  linkedin react 1234567890
  linkedin react 1234567890 --type CELEBRATE
  linkedin engage
  linkedin engage --post --persona ~/nanika/personas/storyteller.md
  linkedin doctor
`)
}
