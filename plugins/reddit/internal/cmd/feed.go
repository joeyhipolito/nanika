package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

// FeedCmd reads the home feed or a subreddit feed.
func FeedCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		return fmt.Errorf("cookies not configured. Run 'reddit configure cookies' first")
	}

	// Parse flags
	limit := 10
	subreddit := ""
	sort := "hot"

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
		case "--subreddit", "-s":
			if i+1 < len(args) {
				i++
				subreddit = normalizeSubreddit(args[i])
			} else {
				return fmt.Errorf("--subreddit requires a value")
			}
		case "--sort":
			if i+1 < len(args) {
				i++
				sort = args[i]
				if sort != "hot" && sort != "new" && sort != "top" && sort != "rising" {
					return fmt.Errorf("--sort must be one of: hot, new, top, rising")
				}
			} else {
				return fmt.Errorf("--sort requires a value")
			}
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
	posts, err := client.Feed(subreddit, sort, limit)
	if err != nil {
		return fmt.Errorf("fetching feed: %w", err)
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

	if subreddit != "" {
		fmt.Printf("r/%s — %s\n", subreddit, sort)
	} else {
		fmt.Printf("Home feed — %s\n", sort)
	}
	fmt.Println()

	printPosts(posts)

	return nil
}
