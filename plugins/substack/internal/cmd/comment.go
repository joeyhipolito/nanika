package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// CommentCmd posts a comment on a specific post.
func CommentCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags and positional args
	var postURL string
	var commentText string
	skipConfirm := false
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--yes", "-y":
			skipConfirm = true
		default:
			if postURL == "" {
				postURL = args[i]
			} else if commentText == "" {
				commentText = args[i]
			} else {
				return fmt.Errorf("unexpected argument: %s", args[i])
			}
		}
	}

	if postURL == "" || commentText == "" {
		return fmt.Errorf("usage: substack comment <post-url> \"comment text\" [--yes] [--json]")
	}

	ref, err := api.ResolvePostURL(postURL)
	if err != nil {
		return fmt.Errorf("parsing post URL: %w", err)
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	var postID int
	var subdomain string
	var postTitle string

	if ref.PostID > 0 {
		postID = ref.PostID
		subdomain = cfg.Subdomain
		postTitle = fmt.Sprintf("Post #%d", postID)
	} else {
		post, err := client.GetPostBySlug(ref.Subdomain, ref.Slug)
		if err != nil {
			return fmt.Errorf("resolving post: %w", err)
		}
		postID = post.ID
		subdomain = ref.Subdomain
		postTitle = post.Title
	}

	// Confirmation prompt
	if !skipConfirm && !jsonOutput {
		fmt.Printf("Post comment on \"%s\"?\n", postTitle)
		fmt.Printf("  Comment: %s\n", commentText)
		fmt.Print("Confirm (y/N): ")

		var confirm string
		fmt.Scanln(&confirm)
		if confirm != "y" && confirm != "Y" {
			fmt.Println("Cancelled.")
			return nil
		}
	}

	comment, err := client.PostComment(subdomain, postID, commentText)
	if err != nil {
		return fmt.Errorf("posting comment: %w", err)
	}

	if jsonOutput {
		type jsonComment struct {
			ID      int    `json:"id"`
			PostID  int    `json:"post_id"`
			Body    string `json:"body"`
			Date    string `json:"date"`
		}
		out := jsonComment{
			ID:     comment.ID,
			PostID: postID,
			Body:   comment.Body,
			Date:   comment.Date,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Println("Comment posted successfully.")
	if comment.Body != "" {
		fmt.Printf("  %s\n", comment.Body)
	} else {
		fmt.Printf("  %s\n", commentText)
	}

	return nil
}
