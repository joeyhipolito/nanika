package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
)

type postsOutput struct {
	Posts []postItem `json:"posts"`
	Count int        `json:"count"`
}

type postItem struct {
	ID         string `json:"id"`
	Commentary string `json:"commentary"`
	Visibility string `json:"visibility"`
	State      string `json:"lifecycle_state"`
	CreatedAt  string `json:"created_at"`
	URL        string `json:"url"`
}

// PostsCmd lists the authenticated user's recent posts.
func PostsCmd(args []string, jsonOutput bool) error {
	// Parse flags
	limit := 10
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--limit":
			if i+1 >= len(args) {
				return fmt.Errorf("--limit requires a number")
			}
			i++
			n := 0
			if _, err := fmt.Sscanf(args[i], "%d", &n); err != nil || n <= 0 {
				return fmt.Errorf("--limit must be a positive integer")
			}
			limit = n
		default:
			return fmt.Errorf("unexpected argument: %s\n\nUsage: linkedin posts [--limit N] [--json]", args[i])
		}
	}

	// Load config and validate
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.AccessToken == "" {
		return fmt.Errorf("OAuth not configured. Run 'linkedin configure' to set up authentication")
	}
	if cfg.PersonURN == "" {
		return fmt.Errorf("person URN missing. Run 'linkedin configure' to re-authorize")
	}

	client := api.NewOAuthClient(cfg.AccessToken, cfg.PersonURN)

	posts, err := client.ListPosts(limit)
	if err != nil {
		return fmt.Errorf("fetching posts: %w", err)
	}

	if jsonOutput {
		items := make([]postItem, len(posts))
		for i, p := range posts {
			items[i] = toPostItem(p)
		}
		out := postsOutput{Posts: items, Count: len(items)}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	if len(posts) == 0 {
		fmt.Println("No posts found.")
		return nil
	}

	fmt.Printf("Your Posts (%d):\n\n", len(posts))
	for _, p := range posts {
		item := toPostItem(p)
		commentary := item.Commentary
		if len(commentary) > 80 {
			commentary = commentary[:77] + "..."
		}
		fmt.Printf("  %s\n", commentary)
		fmt.Printf("    ID:      %s\n", item.ID)
		fmt.Printf("    Date:    %s\n", item.CreatedAt)
		fmt.Printf("    Status:  %s\n", item.State)
		fmt.Printf("    URL:     %s\n\n", item.URL)
	}

	return nil
}

func toPostItem(p api.Post) postItem {
	created := ""
	if p.CreatedAt > 0 {
		created = time.UnixMilli(p.CreatedAt).Format(time.RFC3339)
	}
	return postItem{
		ID:         p.ID,
		Commentary: p.Commentary,
		Visibility: p.Visibility,
		State:      p.LifecycleState,
		CreatedAt:  created,
		URL:        fmt.Sprintf("https://www.linkedin.com/feed/update/%s", p.ID),
	}
}
