package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// DashboardCmd shows the for-you feed — notes and posts from people you follow.
func DashboardCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	limit := 10
	notesOnly := false
	tab := "for-you"
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`substack dashboard - Your for-you feed (notes + posts)

Usage:
  substack dashboard                  Show feed (notes + posts)
  substack dashboard --subscribed     Posts from people you follow
  substack dashboard --notes          Notes only (skip posts)
  substack dashboard --limit 20       More items

Flags:
  --limit <N>        Number of items (default: 10)
  --subscribed       Show subscribed feed instead of for-you
  --notes            Show only notes (skip posts)
  --json             Output in JSON format
`)
			return nil
		case "--limit":
			if i+1 < len(args) {
				i++
				n := parseIntArg(args[i])
				if n > 0 {
					limit = n
				}
			}
		case "--notes":
			notesOnly = true
		case "--subscribed":
			tab = "subscribed"
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)
	items, err := client.GetDashboard(0, tab) // fetch all, filter client-side
	if err != nil {
		return fmt.Errorf("fetching dashboard: %w", err)
	}

	// Filter
	var filtered []api.DashboardItem
	for _, item := range items {
		if notesOnly && item.Type != "comment" {
			continue
		}
		filtered = append(filtered, item)
		if len(filtered) >= limit {
			break
		}
	}

	if jsonOutput {
		type jsonNote struct {
			ID         int    `json:"id"`
			Type       string `json:"type"`
			Body       string `json:"body"`
			Name       string `json:"name"`
			Date       string `json:"date"`
			Reactions  int    `json:"reactions"`
			Replies    int    `json:"replies"`
			CanReply   bool   `json:"can_reply"`
		}
		type jsonPost struct {
			ID          int    `json:"id"`
			Type        string `json:"type"`
			Title       string `json:"title"`
			Publication string `json:"publication"`
			Date        string `json:"date"`
			Comments    int    `json:"comments"`
			Reactions   int    `json:"reactions"`
		}
		var out []any
		for _, item := range filtered {
			if item.Type == "comment" && item.Note != nil {
				out = append(out, jsonNote{
					ID:        item.Note.ID,
					Type:      "note",
					Body:      item.Note.Body,
					Name:      item.Note.Name,
					Date:      item.Note.Date,
					Reactions: item.Note.ReactionCount,
					Replies:   item.Note.ChildrenCount,
					CanReply:  item.CanReply,
				})
			} else if item.Type == "post" && item.Post != nil {
				out = append(out, jsonPost{
					ID:          item.Post.ID,
					Type:        "post",
					Title:       item.Post.Title,
					Publication: item.Publication,
					Date:        item.Post.PostDate,
					Comments:    item.Post.CommentCount,
					Reactions:   item.Post.ReactionCount,
				})
			}
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	if len(filtered) == 0 {
		fmt.Println("No feed items found.")
		return nil
	}

	for _, item := range filtered {
		if item.Type == "comment" && item.Note != nil {
			n := item.Note
			date := n.Date
			if len(date) > 10 {
				date = date[:10]
			}
			body := n.Body
			if len(body) > 200 {
				body = body[:200] + "..."
			}

			fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
			fmt.Printf("📝 %s\n", n.Name)
			fmt.Println(body)
			fmt.Printf("%s · ❤️ %d · 💬 %d · #%d\n", date, n.ReactionCount, n.ChildrenCount, n.ID)
		} else if item.Type == "post" && item.Post != nil {
			p := item.Post
			date := p.PostDate
			if len(date) > 10 {
				date = date[:10]
			}
			fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
			if item.Publication != "" {
				fmt.Printf("📰 %s\n", item.Publication)
			}
			fmt.Println(p.Title)
			fmt.Printf("%s · 💬 %d · ❤️ %d\n", date, p.CommentCount, p.ReactionCount)
			if p.CanonicalURL != "" {
				fmt.Println(p.CanonicalURL)
			}
		}
	}
	fmt.Println("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")

	return nil
}
