package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// CommentsCmd reads comments on a specific post.
func CommentsCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags and positional args
	limit := 25
	var postURL string
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--limit":
			if i+1 < len(args) {
				i++
				n := parseIntArg(args[i])
				if n > 0 {
					limit = n
				}
			}
		default:
			if postURL == "" {
				postURL = args[i]
			} else {
				return fmt.Errorf("unexpected argument: %s", args[i])
			}
		}
	}

	if postURL == "" {
		return fmt.Errorf("usage: substack comments <post-url> [--limit N] [--json]")
	}

	ref, err := api.ResolvePostURL(postURL)
	if err != nil {
		return fmt.Errorf("parsing post URL: %w", err)
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	// Resolve to post ID + subdomain
	var postID int
	var subdomain string
	var postTitle string

	if ref.PostID > 0 {
		// Direct post ID — use configured subdomain
		postID = ref.PostID
		subdomain = cfg.Subdomain
		postTitle = fmt.Sprintf("Post #%d", postID)
	} else {
		// Resolve slug to post
		post, err := client.GetPostBySlug(ref.Subdomain, ref.Slug)
		if err != nil {
			return fmt.Errorf("resolving post: %w", err)
		}
		postID = post.ID
		subdomain = ref.Subdomain
		postTitle = post.Title
	}

	comments, err := client.GetComments(subdomain, postID)
	if err != nil {
		return fmt.Errorf("fetching comments: %w", err)
	}

	// Flatten comment tree for display
	flat := flattenComments(comments, limit)

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(flat)
	}

	if len(flat) == 0 {
		fmt.Println("No comments on this post.")
		return nil
	}

	fmt.Printf("Comments on: %s (%d)\n\n", postTitle, len(flat))
	for _, c := range flat {
		date := c.Date
		if len(date) > 10 {
			date = date[:10]
		}
		fmt.Printf("  %s \u00b7 %s", c.Name, date)
		if c.TotalReactions() > 0 {
			fmt.Printf(" \u00b7 \u2764\ufe0f %d", c.TotalReactions())
		}
		fmt.Println()
		fmt.Printf("    %s\n\n", c.Body)
	}

	return nil
}

// flattenComments flattens a nested comment tree into a flat slice, depth-first.
func flattenComments(comments []api.Comment, limit int) []api.Comment {
	var flat []api.Comment
	for _, c := range comments {
		if len(flat) >= limit {
			break
		}
		flat = append(flat, c)
		if len(c.Children) > 0 {
			childFlat := flattenComments(c.Children, limit-len(flat))
			flat = append(flat, childFlat...)
		}
	}
	if len(flat) > limit {
		flat = flat[:limit]
	}
	return flat
}
