// index.go — RFC §7 Phase 0 (TRK-524): `index rebuild` subcommand.
package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// handleIndexRebuild implements `obsidian index rebuild --vault PATH [--cache PATH]`.
func handleIndexRebuild(args []string) error {
	fs := flag.NewFlagSet("index rebuild", flag.ContinueOnError)
	vaultFlag := fs.String("vault", "", "path to vault (required)")
	cacheFlag := fs.String("cache", "", "path to cache directory (default: <vault>/.cache)")

	if err := fs.Parse(args); err != nil {
		return fmt.Errorf("%w: %v", errUsage, err)
	}

	vaultPath := *vaultFlag
	if vaultPath == "" {
		return fmt.Errorf("%w: --vault is required", errUsage)
	}

	cachePath := *cacheFlag
	if cachePath == "" {
		cachePath = filepath.Join(vaultPath, ".cache")
	}

	if err := index.RebuildEmpty(vaultPath, cachePath); err != nil {
		return fmt.Errorf("index rebuild: %w", err)
	}

	fmt.Fprintf(os.Stdout, "Index rebuilt at %s\n", cachePath)
	return nil
}
