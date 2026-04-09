package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

type logResponse struct {
	Entries []store.Entry `json:"entries"`
	Count   int           `json:"count"`
}

// LogCmd lists recent append-only entries.
func LogCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
		fmt.Print(`Usage: memory log [--limit <n>] [--json]
`)
		return nil
	}

	limit := 10
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--limit":
			i++
			if i >= len(args) {
				return fmt.Errorf("--limit requires a value")
			}
			n, err := parsePositiveInt(args[i], 10)
			if err != nil {
				return err
			}
			limit = n
		default:
			return fmt.Errorf("unknown argument %q", args[i])
		}
	}

	engine, err := store.Open()
	if err != nil {
		return err
	}
	entries := engine.Recent(limit)

	if jsonOutput {
		return output.JSON(logResponse{Entries: entries, Count: len(entries)})
	}

	if len(entries) == 0 {
		fmt.Println("No memory entries yet.")
		return nil
	}
	for _, entry := range entries {
		fmt.Printf("[%d] %s\n", entry.ID, entry.CreatedAt.Format("2006-01-02 15:04:05"))
		fmt.Printf("  %s\n", entry.Text)
	}
	return nil
}
