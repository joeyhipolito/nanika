package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/config"
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
	Configured     bool   `json:"configured"`
	Authenticated  bool   `json:"authenticated"`
	PublicationURL string `json:"publication_url,omitempty"`
	Subdomain      string `json:"subdomain,omitempty"`
}

func queryStatus(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	out := queryStatusOutput{
		Configured:     config.Exists(),
		Authenticated:  cfg.Cookie != "",
		PublicationURL: cfg.PublicationURL,
		Subdomain:      cfg.Subdomain,
	}

	if jsonOutput {
		return encodeJSON(out)
	}

	fmt.Printf("configured:      %v\n", out.Configured)
	fmt.Printf("authenticated:   %v\n", out.Authenticated)
	if out.PublicationURL != "" {
		fmt.Printf("publication_url: %s\n", out.PublicationURL)
	}
	if out.Subdomain != "" {
		fmt.Printf("subdomain:       %s\n", out.Subdomain)
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
	if cfg.PublicationURL != "" {
		items = append(items, queryItemOutput{Type: "publication_url", Value: cfg.PublicationURL})
	}
	if cfg.Subdomain != "" {
		items = append(items, queryItemOutput{Type: "subdomain", Value: cfg.Subdomain})
	}
	if cfg.SiteURL != "" {
		items = append(items, queryItemOutput{Type: "site_url", Value: cfg.SiteURL})
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
		{Name: "draft", Command: "substack draft <file>", Description: "Create a draft from an MDX file"},
		{Name: "drafts", Command: "substack drafts --json", Description: "List current drafts"},
		{Name: "publish", Command: "substack publish <draft-id> --at <RFC3339>", Description: "Schedule a draft for release"},
		{Name: "posts", Command: "substack posts --json", Description: "List published posts"},
		{Name: "feed", Command: "substack feed --json", Description: "Show posts from followed publications"},
		{Name: "note", Command: "substack note <text>", Description: "Post a Note to Substack"},
		{Name: "comment", Command: "substack comment <post-url> <text>", Description: "Post a comment on a post"},
		{Name: "engage", Command: "substack engage --post", Description: "Automated feed engagement"},
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
