package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

type obsidianQueryItem struct {
	Path    string `json:"path"`
	Name    string `json:"name"`
	ModTime int64  `json:"mod_time"`
}

type obsidianQueryItemsOutput struct {
	Items []obsidianQueryItem `json:"items"`
	Count int                 `json:"count"`
}

type obsidianQueryAction struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type obsidianQueryActionsOutput struct {
	Actions []obsidianQueryAction `json:"actions"`
}

// QueryCmd dispatches query subcommands.
// Pass a vault.VaultKind as the optional last argument; omit for KindNanika (backward compat).
func QueryCmd(vaultPath string, subcommand string, jsonOutput bool, kinds ...vault.VaultKind) error {
	switch subcommand {
	case "status":
		return HealthCmd(vaultPath, jsonOutput, kinds...)
	case "items":
		return obsidianQueryItems(vaultPath, jsonOutput, kinds...)
	case "actions":
		return obsidianQueryActions(jsonOutput)
	default:
		return fmt.Errorf("unknown query subcommand %q — use status, items, or actions", subcommand)
	}
}

func obsidianQueryItems(vaultPath string, jsonOutput bool, kinds ...vault.VaultKind) error {
	kind := vault.KindNanika
	if len(kinds) > 0 {
		kind = kinds[0]
	}
	schema := vault.SchemaFor(kind)
	notes, err := vault.ListNotes(vaultPath, schema.Inbox)
	if err != nil {
		return fmt.Errorf("listing notes: %w", err)
	}

	var items []obsidianQueryItem
	for _, info := range notes {
		fullPath := filepath.Join(vaultPath, info.Path)
		data, err := os.ReadFile(fullPath)
		if err != nil {
			continue
		}
		parsed := vault.ParseNote(string(data))
		statusVal, _ := parsed.Frontmatter["status"].(string)
		if statusVal == "processed" {
			continue
		}
		items = append(items, obsidianQueryItem{
			Path:    info.Path,
			Name:    info.Name,
			ModTime: info.ModTime,
		})
	}

	if jsonOutput {
		return output.JSON(obsidianQueryItemsOutput{Items: items, Count: len(items)})
	}

	if len(items) == 0 {
		fmt.Println("Inbox is empty.")
		return nil
	}
	fmt.Printf("Inbox items (%d pending):\n", len(items))
	for _, item := range items {
		fmt.Printf("  %s\n", item.Path)
	}
	return nil
}

func obsidianQueryActions(jsonOutput bool) error {
	actions := []obsidianQueryAction{
		{Name: "capture", Command: "obsidian capture <text>", Description: "Capture a note to inbox"},
		{Name: "read", Command: "obsidian read <path>", Description: "Read a note"},
		{Name: "append", Command: "obsidian append <path> <text>", Description: "Append text to a note"},
		{Name: "create", Command: "obsidian create <path>", Description: "Create a new note"},
		{Name: "search", Command: "obsidian search <query>", Description: "Search notes"},
		{Name: "triage", Command: "obsidian triage", Description: "Triage inbox captures"},
		{Name: "health", Command: "obsidian health", Description: "Report vault health diagnostics"},
		{Name: "ingest", Command: "obsidian ingest", Description: "Ingest content from external sources"},
		{Name: "enrich", Command: "obsidian enrich", Description: "Enrich notes with metadata"},
	}
	if jsonOutput {
		return output.JSON(obsidianQueryActionsOutput{Actions: actions})
	}
	fmt.Println("Available actions:")
	for _, a := range actions {
		fmt.Printf("  %-45s  %s\n", a.Command, a.Description)
	}
	return nil
}
