package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

var validSearchSorts = map[string]bool{
	"relevance": true,
	"new":       true,
	"top":       true,
	"comments":  true,
}

var validTimeFilters = map[string]bool{
	"hour":  true,
	"day":   true,
	"week":  true,
	"month": true,
	"year":  true,
	"all":   true,
}

// SearchCmd searches Reddit posts globally or within a subreddit.
func SearchCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		return fmt.Errorf("cookies not configured. Run 'reddit configure cookies' first")
	}

	// Parse flags
	limit := 25
	subreddit := ""
	sort := "relevance"
	timeFilter := "all"
	var query string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--limit":
			if i+1 >= len(args) {
				return fmt.Errorf("--limit requires a value")
			}
			i++
			n, err := strconv.Atoi(args[i])
			if err != nil || n <= 0 {
				return fmt.Errorf("--limit must be a positive integer")
			}
			limit = n
		case "--subreddit", "-s":
			if i+1 >= len(args) {
				return fmt.Errorf("--subreddit requires a value")
			}
			i++
			subreddit = normalizeSubreddit(args[i])
		case "--sort":
			if i+1 >= len(args) {
				return fmt.Errorf("--sort requires a value")
			}
			i++
			sort = args[i]
			if !validSearchSorts[sort] {
				return fmt.Errorf("--sort must be one of: relevance, new, top, comments")
			}
		case "--time":
			if i+1 >= len(args) {
				return fmt.Errorf("--time requires a value")
			}
			i++
			timeFilter = args[i]
			if !validTimeFilters[timeFilter] {
				return fmt.Errorf("--time must be one of: hour, day, week, month, year, all")
			}
		default:
			if len(args[i]) > 0 && args[i][0] == '-' {
				return fmt.Errorf("unexpected flag: %s", args[i])
			}
			if query != "" {
				return fmt.Errorf("unexpected argument: %s (query already set to %q)", args[i], query)
			}
			query = args[i]
		}
	}

	if query == "" {
		return fmt.Errorf("query is required\n\nUsage: reddit search <query> [--subreddit <sub>] [--sort relevance|new|top|comments] [--time hour|day|week|month|year|all] [--limit N]")
	}

	client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
	posts, err := client.Search(query, subreddit, sort, timeFilter, limit)
	if err != nil {
		return fmt.Errorf("searching: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(posts)
	}

	if len(posts) == 0 {
		fmt.Println("No results found.")
		return nil
	}

	if subreddit != "" {
		fmt.Printf("Search r/%s for %q — %s (%s)\n", subreddit, query, sort, timeFilter)
	} else {
		fmt.Printf("Search Reddit for %q — %s (%s)\n", query, sort, timeFilter)
	}
	fmt.Println()

	printPosts(posts)

	return nil
}
