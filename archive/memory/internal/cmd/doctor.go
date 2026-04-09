package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-memory/internal/config"
	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

type doctorResponse struct {
	OK           bool        `json:"ok"`
	StoreDir     string      `json:"store_dir"`
	LogPath      string      `json:"log_path"`
	SnapshotPath string      `json:"snapshot_path"`
	Stats        store.Stats `json:"stats"`
}

// DoctorCmd validates paths and reports store health.
func DoctorCmd(jsonOutput bool) error {
	engine, err := store.Open()
	if err != nil {
		return err
	}
	stats := engine.Stats()
	resp := doctorResponse{
		OK:           true,
		StoreDir:     config.StoreDir(),
		LogPath:      config.LogPath(),
		SnapshotPath: config.SnapshotPath(),
		Stats:        stats,
	}

	if jsonOutput {
		return output.JSON(resp)
	}

	fmt.Printf("store: %s\n", resp.StoreDir)
	fmt.Printf("entries: %d\n", stats.EntryCount)
	fmt.Printf("entities: %d\n", stats.EntityCount)
	fmt.Printf("tokens: %d\n", stats.TokenCount)
	return nil
}
