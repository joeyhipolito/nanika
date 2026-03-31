package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

// CommentCmd posts a reply to a post or comment.
func CommentCmd(args []string, jsonOutput bool) error {
	for _, arg := range args {
		if arg == "--help" || arg == "-h" {
			fmt.Print(`Usage: reddit comment <post-or-comment-id> "reply text"

Options:
  --json       Output result as JSON
  --help, -h   Show this help
`)
			return nil
		}
	}

	if len(args) < 2 {
		return fmt.Errorf("usage: reddit comment <post-or-comment-id> \"reply text\"")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		return fmt.Errorf("cookies not configured. Run 'reddit configure cookies' first")
	}

	// First arg: parent ID. Default to t3_ (post) if no prefix.
	parentFullname := normalizeFullname(args[0], "t3_")

	// Remaining args form the comment text
	text := strings.Join(args[1:], " ")
	if text == "" {
		return fmt.Errorf("comment text is required")
	}

	client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
	resp, err := client.Comment(parentFullname, text)
	if err != nil {
		return fmt.Errorf("posting comment: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(map[string]interface{}{
			"status": "ok",
			"parent": parentFullname,
			"data":   resp.JSON.Data,
		})
	}

	fmt.Printf("Comment posted on %s\n", parentFullname)

	return nil
}
