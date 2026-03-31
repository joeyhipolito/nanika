package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"time"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

// PostsCmd lists the user's recent posts.
func PostsCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		return fmt.Errorf("cookies not configured. Run 'reddit configure cookies' first")
	}
	if cfg.Username == "" {
		return fmt.Errorf("username not set. Run 'reddit configure cookies' to refresh")
	}

	// Parse flags
	limit := 10
	for i := 0; i < len(args); i++ {
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
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
	posts, err := client.UserPosts(cfg.Username, limit)
	if err != nil {
		return fmt.Errorf("fetching posts: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(posts)
	}

	if len(posts) == 0 {
		fmt.Println("No posts found.")
		return nil
	}

	for _, post := range posts {
		fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
		fmt.Printf("r/%s · %s\n", post.Subreddit, relativeTime(post.CreatedUTC))
		fmt.Printf("%s\n", post.Title)
		if post.IsSelf && post.SelfText != "" {
			text := post.SelfText
			if len(text) > 200 {
				text = text[:197] + "..."
			}
			fmt.Println(text)
		}
		fmt.Printf("↑ %d  💬 %d  ID: %s\n", post.Score, post.NumComments, post.Fullname)
	}
	fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")

	return nil
}

func relativeTime(utc float64) string {
	t := time.Unix(int64(utc), 0)
	d := time.Since(t)

	switch {
	case d < time.Minute:
		return "just now"
	case d < time.Hour:
		return fmt.Sprintf("%dm ago", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%dh ago", int(d.Hours()))
	case d < 30*24*time.Hour:
		return fmt.Sprintf("%dd ago", int(d.Hours()/24))
	default:
		return t.Format("Jan 2, 2006")
	}
}
