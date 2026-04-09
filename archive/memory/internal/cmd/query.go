package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

type queryItem struct {
	ID        uint64 `json:"id"`
	Entity    string `json:"entity,omitempty"`
	Source    string `json:"source,omitempty"`
	CreatedAt string `json:"created_at"`
	Preview   string `json:"preview"`
}

type queryItemsOutput struct {
	Items []queryItem `json:"items"`
	Count int         `json:"count"`
}

type queryAction struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type queryActionsOutput struct {
	Actions []queryAction `json:"actions"`
}

// QueryCmd serves the dashboard query protocol.
func QueryCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		fmt.Print(`Usage: memory query <status|items|actions|action rebuild> [--json]
`)
		return nil
	}

	switch args[0] {
	case "status":
		engine, err := store.Open()
		if err != nil {
			return err
		}
		return outputOrText(engine.Stats(), jsonOutput)
	case "items":
		engine, err := store.Open()
		if err != nil {
			return err
		}
		entries := engine.Recent(10)
		items := make([]queryItem, 0, len(entries))
		for _, entry := range entries {
			items = append(items, queryItem{
				ID:        entry.ID,
				Entity:    entry.Entity,
				Source:    entry.Source,
				CreatedAt: entry.CreatedAt.Format("2006-01-02T15:04:05Z07:00"),
				Preview:   entry.Text,
			})
		}
		out := queryItemsOutput{Items: items, Count: len(items)}
		if jsonOutput {
			return output.JSON(out)
		}
		for _, item := range out.Items {
			fmt.Printf("[%d] %s\n", item.ID, item.Preview)
		}
		return nil
	case "actions":
		actions := queryActionsOutput{
			Actions: []queryAction{
				{Name: "add", Command: "memory add <text>", Description: "Append a free-form memory entry"},
				{Name: "remember", Command: "memory remember <entity> --slot <key=value>", Description: "Update entity state directly"},
				{Name: "find", Command: "memory find <query>", Description: "Search the compiled symbolic index"},
				{Name: "state", Command: "memory state <entity>", Description: "Show the current entity state"},
				{Name: "rebuild", Command: "memory rebuild", Description: "Rebuild the compiled snapshot from the log"},
			},
		}
		if jsonOutput {
			return output.JSON(actions)
		}
		for _, action := range actions.Actions {
			fmt.Printf("%s: %s\n", action.Command, action.Description)
		}
		return nil
	case "action":
		if len(args) < 2 {
			return fmt.Errorf("query action requires a subcommand")
		}
		switch args[1] {
		case "rebuild":
			return RebuildCmd(jsonOutput)
		default:
			return fmt.Errorf("unknown action %q", args[1])
		}
	default:
		return fmt.Errorf("unknown query subcommand %q", args[0])
	}
}

func outputOrText(stats store.Stats, jsonOutput bool) error {
	if jsonOutput {
		return output.JSON(stats)
	}
	fmt.Printf("entries=%d entities=%d tokens=%d\n", stats.EntryCount, stats.EntityCount, stats.TokenCount)
	return nil
}
