package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

// CommentsCmd reads the comment tree for a post.
func CommentsCmd(args []string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("usage: reddit comments <post-id-or-url> [--limit N] [--sort best]")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		return fmt.Errorf("cookies not configured. Run 'reddit configure cookies' first")
	}

	// First arg is the post ID
	postID := normalizePostID(args[0])

	// Parse remaining flags
	limit := 25
	depth := 5
	sort := "best"

	for i := 1; i < len(args); i++ {
		switch args[i] {
		case "--limit":
			if i+1 < len(args) {
				i++
				n, err := strconv.Atoi(args[i])
				if err != nil || n <= 0 {
					return fmt.Errorf("--limit must be a positive integer")
				}
				limit = n
			} else {
				return fmt.Errorf("--limit requires a value")
			}
		case "--depth":
			if i+1 < len(args) {
				i++
				n, err := strconv.Atoi(args[i])
				if err != nil || n <= 0 {
					return fmt.Errorf("--depth must be a positive integer")
				}
				depth = n
			} else {
				return fmt.Errorf("--depth requires a value")
			}
		case "--sort":
			if i+1 < len(args) {
				i++
				sort = args[i]
			} else {
				return fmt.Errorf("--sort requires a value")
			}
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
	posts, comments, err := client.Comments(postID, sort, limit, depth)
	if err != nil {
		return fmt.Errorf("fetching comments: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(map[string]interface{}{
			"post":     posts,
			"comments": comments,
		})
	}

	// Show the post context
	if len(posts) > 0 {
		p := posts[0]
		fmt.Printf("r/%s · u/%s · %s\n", p.Subreddit, p.Author, relativeTime(p.CreatedUTC))
		fmt.Printf("%s\n", p.Title)
		if p.IsSelf && p.SelfText != "" {
			text := p.SelfText
			if len(text) > 400 {
				text = text[:397] + "..."
			}
			fmt.Println(text)
		}
		fmt.Printf("↑ %d  💬 %d\n", p.Score, p.NumComments)
		fmt.Println()
	}

	if len(comments) == 0 {
		fmt.Println("No comments.")
		return nil
	}

	fmt.Printf("Comments (%d):\n", len(comments))
	fmt.Println()

	for _, c := range comments {
		indent := strings.Repeat("  ", c.Depth)
		body := c.Body
		if len(body) > 300 {
			body = body[:297] + "..."
		}
		fmt.Printf("%su/%s · ↑ %d · %s\n", indent, c.Author, c.Score, relativeTime(c.CreatedUTC))
		// Indent each line of the body
		for _, line := range strings.Split(body, "\n") {
			fmt.Printf("%s  %s\n", indent, line)
		}
		fmt.Printf("%s  ID: %s\n", indent, c.Fullname)
		fmt.Println()
	}

	return nil
}
