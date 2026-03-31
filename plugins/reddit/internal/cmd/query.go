package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

// QueryCmd routes query subcommands: status, items, actions.
func QueryCmd(subcommand string, jsonOutput bool) error {
	switch subcommand {
	case "status":
		return queryStatus(jsonOutput)
	case "items":
		return queryItems(jsonOutput)
	case "actions":
		return queryActions(jsonOutput)
	default:
		return fmt.Errorf("unknown query subcommand %q — use status, items, or actions", subcommand)
	}
}

// queryStatusOutput is the JSON shape for `query status --json`.
type queryStatusOutput struct {
	Configured bool   `json:"configured"`
	HasSession bool   `json:"has_session"`
	HasCSRF    bool   `json:"has_csrf"`
	Username   string `json:"username,omitempty"`
}

func queryStatus(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	out := queryStatusOutput{
		Configured: config.Exists(),
		HasSession: cfg.RedditSession != "",
		HasCSRF:    cfg.CSRFToken != "",
		Username:   cfg.Username,
	}

	if jsonOutput {
		return encodeJSON(out)
	}

	fmt.Printf("configured:  %v\n", out.Configured)
	fmt.Printf("has_session: %v\n", out.HasSession)
	fmt.Printf("has_csrf:    %v\n", out.HasCSRF)
	if out.Username != "" {
		fmt.Printf("username:    %s\n", out.Username)
	}
	return nil
}

// queryItemOutput is one element in the JSON array for `query items --json`.
type queryItemOutput struct {
	Type  string `json:"type"`
	Value string `json:"value"`
}

type queryItemsOutput struct {
	Items []queryItemOutput `json:"items"`
	Count int               `json:"count"`
}

func queryItems(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	items := []queryItemOutput{}
	if cfg.Username != "" {
		items = append(items, queryItemOutput{Type: "username", Value: cfg.Username})
	}

	if jsonOutput {
		return encodeJSON(queryItemsOutput{Items: items, Count: len(items)})
	}

	if len(items) == 0 {
		fmt.Println("No items configured.")
		return nil
	}
	for _, it := range items {
		fmt.Printf("type=%-16s  value=%s\n", it.Type, it.Value)
	}
	return nil
}

// queryActionItem is one element in the JSON array for `query actions --json`.
type queryActionItem struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type queryActionsOutput struct {
	Actions []queryActionItem `json:"actions"`
}

func queryActions(jsonOutput bool) error {
	actions := []queryActionItem{
		{Name: "post", Command: "reddit post --subreddit <sub> --title <title> <body>", Description: "Submit a text post"},
		{Name: "post-link", Command: "reddit post --subreddit <sub> --title <title> --url <url>", Description: "Submit a link post"},
		{Name: "posts", Command: "reddit posts --json", Description: "List your recent posts"},
		{Name: "feed", Command: "reddit feed --json", Description: "Show your home feed"},
		{Name: "search", Command: "reddit search <query> --json", Description: "Search Reddit posts"},
		{Name: "comment", Command: "reddit comment <id> <text>", Description: "Reply to a post or comment"},
		{Name: "vote", Command: "reddit vote <id>", Description: "Upvote a post or comment"},
	}

	if jsonOutput {
		return encodeJSON(queryActionsOutput{Actions: actions})
	}

	for _, a := range actions {
		fmt.Printf("%-12s  %s\n              command: %s\n", a.Name, a.Description, a.Command)
	}
	return nil
}

// encodeJSON writes v to stdout as indented JSON.
func encodeJSON(v any) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}
