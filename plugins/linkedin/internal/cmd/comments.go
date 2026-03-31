package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/browser"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
)

// CommentsCmd reads comments on a post. Tries Official API first, falls back to CDP.
func CommentsCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: linkedin comments <activity-urn-or-id> [--limit N]")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	urn := normalizeActivityURN(args[0])
	remainingArgs := args[1:]

	// Parse flags
	limit := 10
	for i := 0; i < len(remainingArgs); i++ {
		switch remainingArgs[i] {
		case "--limit":
			if i+1 < len(remainingArgs) {
				i++
				n, err := strconv.Atoi(remainingArgs[i])
				if err != nil || n <= 0 {
					return fmt.Errorf("--limit must be a positive integer")
				}
				limit = n
			} else {
				return fmt.Errorf("--limit requires a value")
			}
		default:
			return fmt.Errorf("unexpected argument: %s", remainingArgs[i])
		}
	}

	// Try Official API first (if OAuth is configured)
	if cfg.AccessToken != "" && cfg.PersonURN != "" {
		client := api.NewOAuthClient(cfg.AccessToken, cfg.PersonURN)
		officialComments, err := client.GetCommentsOfficial(urn, limit)
		if err == nil {
			return renderOfficialComments(officialComments, jsonOutput)
		}
		// Fall through to CDP on error
	}

	// Fallback to CDP
	cdp := browser.NewCDPClient(cfg.ChromeDebugURL)
	comments, err := cdp.GetComments(urn, limit)
	if err != nil {
		return fmt.Errorf("fetching comments: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(comments)
	}

	if len(comments) == 0 {
		fmt.Println("No comments found.")
		return nil
	}

	fmt.Printf("Comments on %s:\n\n", urn)
	for _, c := range comments {
		author := c.AuthorName
		if author == "" {
			author = "Unknown"
		}
		if c.Timestamp != "" {
			fmt.Printf("  %s · %s\n", author, c.Timestamp)
		} else {
			fmt.Printf("  %s\n", author)
		}
		fmt.Printf("  %s\n", c.Text)
		if c.Reactions > 0 {
			fmt.Printf("  👍 %d\n", c.Reactions)
		}
		fmt.Println()
	}

	return nil
}

func renderOfficialComments(comments []api.OfficialComment, jsonOutput bool) error {
	var items []api.Comment
	for _, c := range comments {
		items = append(items, api.Comment{
			AuthorName: c.Actor,
			Text:       c.Message.Text,
			Reactions:  0,
		})
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(items)
	}

	if len(items) == 0 {
		fmt.Println("No comments found.")
		return nil
	}

	for _, c := range items {
		author := c.AuthorName
		if author == "" {
			author = "Unknown"
		}
		fmt.Printf("  %s\n", author)
		fmt.Printf("  %s\n\n", c.Text)
	}

	return nil
}
