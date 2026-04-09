package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

// RebuildCmd rebuilds the compiled snapshot from the append-only log.
func RebuildCmd(jsonOutput bool) error {
	engine, err := store.Rebuild()
	if err != nil {
		return err
	}
	stats := engine.Stats()

	if jsonOutput {
		return output.JSON(stats)
	}

	fmt.Printf("rebuilt %d entries across %d entities\n", stats.EntryCount, stats.EntityCount)
	return nil
}
