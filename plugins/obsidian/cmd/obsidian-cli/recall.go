package main

import (
	"flag"
	"fmt"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
)

// handleRecallCommand dispatches the recall command with socket fallback logic.
func handleRecallCommand(vaultPath string, args []string, jsonOutput bool) error {
	fs := flag.NewFlagSet("recall", flag.ContinueOnError)
	seedFlag := fs.String("seed", "", "seed note path (required)")
	limitFlag := fs.Int("limit", 5, "max results (default 5, clamped to 1-1000)")
	formatFlag := fs.String("format", "json", "output format: json, markdown, paths, brief")
	socketFlag := fs.String("socket", "", "path to recall RPC socket (default: from config)")
	timeoutFlag := fs.Duration("timeout", 5*time.Second, "socket connection timeout")
	noFallbackFlag := fs.Bool("no-fallback", false, "disable fallback to in-process recall")

	if err := fs.Parse(args); err != nil {
		return fmt.Errorf("%w: %v", errUsage, err)
	}

	// Seed can be positional argument if not provided via --seed
	seed := *seedFlag
	if seed == "" && len(fs.Args()) > 0 {
		seed = fs.Args()[0]
	}
	if seed == "" {
		return fmt.Errorf("%w: seed note path is required (--seed <path> or positional argument)", errUsage)
	}

	// Validate format
	if !isValidFormat(*formatFlag) {
		return fmt.Errorf("invalid format: %q (valid: json, markdown, paths, brief)", *formatFlag)
	}

	// Clamp limit to valid range
	limit := *limitFlag
	if limit < 1 {
		limit = 1
	}
	if limit > 1000 {
		limit = 1000
	}

	// Resolve socket path
	socketPath := *socketFlag
	if socketPath == "" {
		socketPath = config.ResolveRecallSocket()
	}

	// Execute recall query with fallback logic
	results, err := connectOrFallback(socketPath, seed, limit, *timeoutFlag, *noFallbackFlag, vaultPath)
	if err != nil {
		return fmt.Errorf("recall: %w", err)
	}

	// Format and output
	output, err := formatRecall(results, *formatFlag)
	if err != nil {
		return fmt.Errorf("recall format: %w", err)
	}

	fmt.Fprint(os.Stdout, output)
	return nil
}

// isValidFormat checks if the format is recognized.
func isValidFormat(format string) bool {
	switch format {
	case "json", "markdown", "paths", "brief":
		return true
	}
	return false
}
