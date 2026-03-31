package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"

	"github.com/joeyhipolito/nanika-linkedin/internal/browser"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
)

// FeedCmd reads the user's LinkedIn feed via Chrome CDP.
func FeedCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
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

	cdp := browser.NewCDPClient(cfg.ChromeDebugURL)
	items, err := cdp.GetFeed(limit)
	if err != nil {
		return fmt.Errorf("fetching feed: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(items)
	}

	if len(items) == 0 {
		fmt.Println("No feed items found.")
		return nil
	}

	for _, item := range items {
		fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")

		author := item.AuthorName
		if author == "" {
			author = "Unknown"
		}
		if item.Timestamp != "" {
			fmt.Printf("%s · %s\n", author, item.Timestamp)
		} else {
			fmt.Println(author)
		}

		text := item.Text
		if len(text) > 280 {
			text = text[:277] + "..."
		}
		fmt.Println(text)

		fmt.Printf("👍 %d  💬 %d  🔄 %d\n", item.ReactionCount, item.CommentCount, item.RepostCount)
		fmt.Printf("ID: %s\n", item.ActivityURN)
	}
	fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")

	return nil
}
