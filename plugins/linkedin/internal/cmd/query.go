package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-linkedin/internal/config"
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
	Configured            bool   `json:"configured"`
	Authenticated         bool   `json:"authenticated"`
	PersonURN             string `json:"person_urn,omitempty"`
	TokenExpiry           string `json:"token_expiry,omitempty"`
	ChromeDebugConfigured bool   `json:"chrome_debug_configured"`
}

func queryStatus(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	out := queryStatusOutput{
		Configured:            config.Exists(),
		Authenticated:         cfg.AccessToken != "",
		PersonURN:             cfg.PersonURN,
		TokenExpiry:           cfg.TokenExpiry,
		ChromeDebugConfigured: cfg.ChromeDebugURL != "",
	}

	if jsonOutput {
		return encodeJSON(out)
	}

	fmt.Printf("configured:     %v\n", out.Configured)
	fmt.Printf("authenticated:  %v\n", out.Authenticated)
	fmt.Printf("chrome_debug:   %v\n", out.ChromeDebugConfigured)
	if out.PersonURN != "" {
		fmt.Printf("person_urn:     %s\n", out.PersonURN)
	}
	if out.TokenExpiry != "" {
		fmt.Printf("token_expiry:   %s\n", out.TokenExpiry)
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
	if cfg.PersonURN != "" {
		items = append(items, queryItemOutput{Type: "person_urn", Value: cfg.PersonURN})
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
		{Name: "post", Command: "linkedin post <text>", Description: "Create a text post"},
		{Name: "post-file", Command: "linkedin post --file <mdx>", Description: "Create a post from an MDX file"},
		{Name: "posts", Command: "linkedin posts --json", Description: "List your recent posts"},
		{Name: "feed", Command: "linkedin feed --json", Description: "Show your LinkedIn feed"},
		{Name: "comment", Command: "linkedin comment <urn> <text>", Description: "Comment on a post"},
		{Name: "react", Command: "linkedin react <urn>", Description: "React to a post"},
		{Name: "engage", Command: "linkedin engage --post", Description: "Automated feed engagement"},
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
