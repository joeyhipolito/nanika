package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-youtube/internal/api"
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
	Configured    bool `json:"configured"`
	HasAPIKey     bool `json:"has_api_key"`
	Authenticated bool `json:"authenticated"`
	ChannelCount  int  `json:"channel_count"`
	Budget        int  `json:"budget"`
}

func queryStatus(jsonOutput bool) error {
	out := queryStatusOutput{}

	cfg, err := api.LoadConfig()
	if err == nil {
		out.Configured = true
		out.HasAPIKey = cfg.APIKey != ""
		out.ChannelCount = len(cfg.Channels)
		out.Budget = cfg.Budget

		tok, terr := api.LoadToken()
		if terr == nil && tok != nil {
			out.Authenticated = !tok.IsExpired()
		}
	}

	if jsonOutput {
		return encodeJSON(out)
	}

	fmt.Printf("configured:    %v\n", out.Configured)
	fmt.Printf("has_api_key:   %v\n", out.HasAPIKey)
	fmt.Printf("authenticated: %v\n", out.Authenticated)
	fmt.Printf("channels:      %d\n", out.ChannelCount)
	fmt.Printf("budget:        %d\n", out.Budget)
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
	items := []queryItemOutput{}

	cfg, err := api.LoadConfig()
	if err == nil {
		for _, ch := range cfg.Channels {
			items = append(items, queryItemOutput{Type: "channel", Value: ch})
		}
	}

	if jsonOutput {
		return encodeJSON(queryItemsOutput{Items: items, Count: len(items)})
	}

	if len(items) == 0 {
		fmt.Println("No channels configured.")
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
		{Name: "scan", Command: "youtube scan --json", Description: "Scan configured channels for recent videos"},
		{Name: "scan-topics", Command: "youtube scan --topics <topics> --json", Description: "Search videos by topic"},
		{Name: "comment", Command: "youtube comment <video-id> <text>", Description: "Post a top-level comment (50 quota units)"},
		{Name: "like", Command: "youtube like <video-id>", Description: "Like a video (50 quota units)"},
	}

	if jsonOutput {
		return encodeJSON(queryActionsOutput{Actions: actions})
	}

	for _, a := range actions {
		fmt.Printf("%-14s  %s\n                command: %s\n", a.Name, a.Description, a.Command)
	}
	return nil
}

// encodeJSON writes v to stdout as indented JSON.
func encodeJSON(v any) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}
