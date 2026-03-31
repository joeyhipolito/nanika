package cmd

import (
	"crypto/md5"
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// PostsCmd lists published posts.
func PostsCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags
	limit := 25
	offset := 0
	scoutOutput := false
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
		case "--offset":
			if i+1 < len(args) {
				i++
				n := parseIntArg(args[i])
				if n >= 0 {
					offset = n
				}
			}
		case "--scout":
			scoutOutput = true
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)
	posts, err := client.GetPosts(offset, limit)
	if err != nil {
		return fmt.Errorf("fetching posts: %w", err)
	}

	if scoutOutput {
		out := postsToScout(posts, cfg.PublicationURL)
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(posts)
	}

	if len(posts) == 0 {
		fmt.Println("No published posts found.")
		return nil
	}

	fmt.Printf("Published Posts (%d):\n\n", len(posts))
	for _, p := range posts {
		date := p.PublishDate
		if date == "" {
			date = p.PostDate
		}
		if date == "" {
			date = "no date"
		}
		url := fmt.Sprintf("%s/p/%s", cfg.PublicationURL, p.Slug)
		fmt.Printf("  %s\n", p.Title)
		fmt.Printf("    Date:     %s\n", date)
		fmt.Printf("    Audience: %s\n", p.Audience)
		fmt.Printf("    URL:      %s\n\n", url)
	}

	return nil
}

// postsToScout converts published Posts to ScoutItems for scout pipeline integration.
func postsToScout(posts []api.Post, pubURL string) []ScoutItem {
	out := make([]ScoutItem, 0, len(posts))
	for _, p := range posts {
		postURL := fmt.Sprintf("%s/p/%s", pubURL, p.Slug)
		if p.CanonicalURL != "" {
			postURL = p.CanonicalURL
		}
		date := p.PublishDate
		if date == "" {
			date = p.PostDate
		}
		ts := parsePostDate(date)
		content := p.Description
		if content == "" {
			content = p.Subtitle
		}
		tags := []string{"substack"}
		for _, pt := range p.PostTags {
			tags = append(tags, pt.Name)
		}
		out = append(out, ScoutItem{
			ID:         fmt.Sprintf("%x", md5.Sum([]byte(postURL)))[:16],
			Title:      p.Title,
			Content:    content,
			SourceURL:  postURL,
			Timestamp:  ts,
			Tags:       tags,
			Engagement: p.CommentCount + p.ReactionCount,
		})
	}
	return out
}
