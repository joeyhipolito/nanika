package cmd

import (
	"fmt"
	"strconv"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

type trustResponse struct {
	OK      bool        `json:"ok"`
	Entry   store.Entry `json:"entry"`
	Signals []string    `json:"signals"`
}

// TrustCmd records a trust signal for an existing memory entry.
// Usage: memory trust <id> <signal>
func TrustCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
		fmt.Print(`Usage: memory trust <id> <signal>

Records a trust signal for an existing memory entry.

Arguments:
  <id>      Numeric entry ID (see 'memory log' for IDs)
  <signal>  Trust signal: helpful | unhelpful

Signals:
  helpful    Marks this entry as useful; trust score +0.05
  unhelpful  Marks this entry as not useful; trust score -0.10

Options:
  --json    Output machine-readable JSON

Examples:
  memory trust 42 helpful
  memory trust 42 unhelpful
  memory trust 42 helpful --json
`)
		return nil
	}

	if len(args) < 2 {
		return fmt.Errorf("usage: memory trust <id> <signal>")
	}

	rawID := args[0]
	signal := args[1]

	id, err := strconv.ParseUint(rawID, 10, 64)
	if err != nil || id == 0 {
		return fmt.Errorf("invalid entry id %q: must be a positive integer", rawID)
	}

	engine, err := store.Open()
	if err != nil {
		return err
	}

	entry, err := engine.Trust(id, signal)
	if err != nil {
		return err
	}

	signals := engine.TrustSignals(id)

	if jsonOutput {
		return output.JSON(trustResponse{
			OK:      true,
			Entry:   entry,
			Signals: signals,
		})
	}

	fmt.Printf("recorded %s for entry %d\n", signal, id)
	fmt.Printf("signals: %v\n", signals)
	return nil
}
