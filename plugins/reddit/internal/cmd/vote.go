package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

// VoteCmd upvotes or downvotes a post or comment.
func VoteCmd(args []string, jsonOutput bool) error {
	for _, arg := range args {
		if arg == "--help" || arg == "-h" {
			fmt.Print(`Usage: reddit vote <post-or-comment-id> [--down] [--unvote]

Options:
  --down       Downvote (default: upvote)
  --unvote     Remove existing vote
  --json       Output result as JSON
  --help, -h   Show this help
`)
			return nil
		}
	}

	if len(args) < 1 {
		return fmt.Errorf("usage: reddit vote <post-or-comment-id> [--down] [--unvote]")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		return fmt.Errorf("cookies not configured. Run 'reddit configure cookies' first")
	}

	// First arg: ID. Default to t3_ (post) if no prefix.
	fullname := normalizeFullname(args[0], "t3_")

	// Parse flags
	dir := 1 // default: upvote
	for i := 1; i < len(args); i++ {
		switch args[i] {
		case "--down", "-d":
			dir = -1
		case "--unvote":
			dir = 0
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
	if err := client.Vote(fullname, dir); err != nil {
		return fmt.Errorf("voting: %w", err)
	}

	action := "Upvoted"
	if dir == -1 {
		action = "Downvoted"
	} else if dir == 0 {
		action = "Unvoted"
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(map[string]interface{}{
			"status":   "ok",
			"action":   action,
			"fullname": fullname,
		})
	}

	fmt.Printf("%s %s\n", action, fullname)

	return nil
}
