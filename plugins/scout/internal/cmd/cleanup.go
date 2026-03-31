package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
)

// CleanupCmd removes intel files older than a configurable TTL.
func CleanupCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	var olderStr string
	dryRun := false

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--older":
			if i+1 < len(args) {
				i++
				olderStr = args[i]
			} else {
				return fmt.Errorf("--older requires a duration argument (e.g. --older 30d)")
			}
		case "--dry-run":
			dryRun = true
		default:
			return fmt.Errorf("unknown flag: %s\n\nUsage: scout cleanup [--older <duration>] [--dry-run]", args[i])
		}
	}

	// Safe default: if no --older provided, dry-run with 30d TTL.
	if olderStr == "" {
		olderStr = "30d"
		dryRun = true
	}

	ttl, err := parseDuration(olderStr)
	if err != nil {
		return fmt.Errorf("invalid --older value %q: %w", olderStr, err)
	}

	cutoff := time.Now().Add(-ttl)
	today := time.Now().Truncate(24 * time.Hour)

	candidates, err := findOldIntelFiles(config.IntelDir(), cutoff, today)
	if err != nil {
		return err
	}

	if len(candidates) == 0 {
		if jsonOutput {
			fmt.Println(`{"files_removed":0,"bytes_freed":0}`)
			return nil
		}
		if dryRun {
			fmt.Printf("Dry run: no files older than %s found.\n", olderStr)
		} else {
			fmt.Println("No files to remove.")
		}
		return nil
	}

	var totalBytes int64
	for _, c := range candidates {
		totalBytes += c.size
	}

	if dryRun {
		return printDryRun(candidates, totalBytes, olderStr, cutoff, jsonOutput)
	}

	return deleteFiles(candidates, totalBytes, jsonOutput)
}

type intelFileCandidate struct {
	path  string
	topic string
	size  int64
}

// findOldIntelFiles walks the intel directory and collects files that are
// older than cutoff but not from today.
func findOldIntelFiles(intelDir string, cutoff, today time.Time) ([]intelFileCandidate, error) {
	topicEntries, err := os.ReadDir(intelDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("failed to read intel directory: %w", err)
	}

	var candidates []intelFileCandidate
	for _, topicEntry := range topicEntries {
		if !topicEntry.IsDir() {
			continue
		}
		topicDir := filepath.Join(intelDir, topicEntry.Name())
		fileEntries, err := os.ReadDir(topicDir)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to read %s: %v\n", topicDir, err)
			continue
		}

		for _, fileEntry := range fileEntries {
			if fileEntry.IsDir() || !strings.HasSuffix(fileEntry.Name(), ".json") {
				continue
			}

			info, err := fileEntry.Info()
			if err != nil {
				continue
			}

			mtime := info.ModTime()

			// Never delete files from today.
			if mtime.Truncate(24 * time.Hour).Equal(today) {
				continue
			}

			if mtime.Before(cutoff) {
				candidates = append(candidates, intelFileCandidate{
					path:  filepath.Join(topicDir, fileEntry.Name()),
					topic: topicEntry.Name(),
					size:  info.Size(),
				})
			}
		}
	}
	return candidates, nil
}

func printDryRun(candidates []intelFileCandidate, totalBytes int64, olderStr string, cutoff time.Time, jsonOutput bool) error {
	if jsonOutput {
		type fileEntry struct {
			Path  string `json:"path"`
			Topic string `json:"topic"`
			Size  int64  `json:"size"`
		}
		type result struct {
			DryRun     bool        `json:"dry_run"`
			FilesFound int         `json:"files_found"`
			BytesFound int64       `json:"bytes_found"`
			Files      []fileEntry `json:"files"`
		}
		files := make([]fileEntry, len(candidates))
		for i, c := range candidates {
			files[i] = fileEntry{Path: c.path, Topic: c.topic, Size: c.size}
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(result{
			DryRun:     true,
			FilesFound: len(candidates),
			BytesFound: totalBytes,
			Files:      files,
		})
	}

	fmt.Printf("Dry run: %d files would be removed (%s freed)\n", len(candidates), formatBytes(totalBytes))
	fmt.Printf("Older than: %s (cutoff: %s)\n\n", olderStr, cutoff.Format("2006-01-02"))
	for _, c := range candidates {
		fmt.Printf("  [%s] %s\n", c.topic, filepath.Base(c.path))
	}
	fmt.Println("\nTo delete, run:")
	fmt.Printf("  scout cleanup --older %s\n", olderStr)
	return nil
}

func deleteFiles(candidates []intelFileCandidate, totalBytes int64, jsonOutput bool) error {
	var removed int
	var freed int64
	for _, c := range candidates {
		if err := os.Remove(c.path); err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to remove %s: %v\n", c.path, err)
			continue
		}
		removed++
		freed += c.size
	}

	if jsonOutput {
		type result struct {
			FilesRemoved int   `json:"files_removed"`
			BytesFreed   int64 `json:"bytes_freed"`
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(result{FilesRemoved: removed, BytesFreed: freed})
	}

	fmt.Printf("Cleanup complete: %d files removed, %s freed\n", removed, formatBytes(freed))
	return nil
}

// formatBytes formats a byte count as a human-readable string.
func formatBytes(b int64) string {
	if b < 1024 {
		return fmt.Sprintf("%d B", b)
	}
	div, exp := int64(1024), 0
	for n := b / 1024; n >= 1024; n /= 1024 {
		div *= 1024
		exp++
	}
	return fmt.Sprintf("%.1f %cB", float64(b)/float64(div), "KMGTPE"[exp])
}
