package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

// PostCmd submits a new post to Reddit.
func PostCmd(args []string, jsonOutput bool) error {
	for _, arg := range args {
		if arg == "--help" || arg == "-h" {
			fmt.Print(`Usage: reddit post --subreddit <sub> --title "Title" "body text"
       reddit post --subreddit <sub> --title "Title" --url <url>

Options:
  --subreddit, -s <sub>   Target subreddit (required)
  --title, -t <title>     Post title (required)
  --url, -u <url>         Link URL (for link posts)
  --json                  Output result as JSON
  --help, -h              Show this help
`)
			return nil
		}
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		return fmt.Errorf("cookies not configured. Run 'reddit configure cookies' first")
	}

	// Parse flags
	var subreddit, title, linkURL string
	var textParts []string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--subreddit", "-s":
			if i+1 < len(args) {
				i++
				subreddit = normalizeSubreddit(args[i])
			} else {
				return fmt.Errorf("--subreddit requires a value")
			}
		case "--title", "-t":
			if i+1 < len(args) {
				i++
				title = args[i]
			} else {
				return fmt.Errorf("--title requires a value")
			}
		case "--url", "-u":
			if i+1 < len(args) {
				i++
				linkURL = args[i]
			} else {
				return fmt.Errorf("--url requires a value")
			}
		default:
			textParts = append(textParts, args[i])
		}
	}

	if subreddit == "" {
		return fmt.Errorf("--subreddit is required. Usage: reddit post --subreddit golang --title \"Title\" \"body text\"")
	}
	if title == "" {
		return fmt.Errorf("--title is required. Usage: reddit post --subreddit golang --title \"Title\" \"body text\"")
	}

	// Determine post kind
	kind := "self"
	text := strings.Join(textParts, " ")
	if linkURL != "" {
		kind = "link"
	} else if text == "" {
		return fmt.Errorf("post body or --url is required")
	}

	client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
	resp, err := client.Submit(subreddit, title, text, linkURL, kind)
	if err != nil {
		return fmt.Errorf("submitting post: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(map[string]string{
			"status": "ok",
			"id":     resp.JSON.Data.ID,
			"name":   resp.JSON.Data.Name,
			"url":    resp.JSON.Data.URL,
		})
	}

	fmt.Printf("Post submitted to r/%s\n", subreddit)
	fmt.Printf("  ID:  %s\n", resp.JSON.Data.Name)
	fmt.Printf("  URL: %s\n", resp.JSON.Data.URL)

	return nil
}
