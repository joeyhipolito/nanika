package cmd

import (
	"fmt"
	"sort"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

// StateCmd prints the current slot state for one entity.
func StateCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		fmt.Print(`Usage: memory state <entity> [--json]

Examples:
  memory state Alice
  memory state "Project Atlas" --json
`)
		return nil
	}

	engine, err := store.Open()
	if err != nil {
		return err
	}
	state, ok := engine.State(args[0])
	if !ok {
		return fmt.Errorf("no state found for %q", args[0])
	}

	if jsonOutput {
		return output.JSON(state)
	}

	fmt.Printf("%s\n", state.Entity)
	keys := make([]string, 0, len(state.Slots))
	for key := range state.Slots {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		fmt.Printf("%s=%s\n", key, state.Slots[key])
	}
	return nil
}
