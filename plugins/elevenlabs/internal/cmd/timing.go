package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
)

// TimingCmd reads a timing-map.json and produces a human-readable clip alignment guide.
//
// Usage: elevenlabs timing <timing-map.json> [--json]
//
// Flags:
//   --json   Output as structured JSON for programmatic use
func TimingCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 {
		return fmt.Errorf("missing required argument: <timing-map.json>")
	}
	timingPath := args[0]

	// Read and parse timing map.
	data, err := os.ReadFile(timingPath)
	if err != nil {
		return fmt.Errorf("reading %s: %w", timingPath, err)
	}

	var tm api.TimingMap
	if err := json.Unmarshal(data, &tm); err != nil {
		return fmt.Errorf("parsing %s: %w", timingPath, err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(tm)
	}

	// Print human-readable guide.
	fmt.Println("Clip Alignment Guide")
	fmt.Println("====================")
	fmt.Println()
	fmt.Printf("Audio File: %s\n", tm.AudioFile)
	fmt.Printf("Duration:   %.2fs\n", tm.DurationSeconds)
	fmt.Println()

	if len(tm.Words) > 0 {
		fmt.Println("WORDS")
		fmt.Println("-----")
		for i, w := range tm.Words {
			fmt.Printf("%3d. [%7.2fs - %7.2fs] %s\n", i+1, w.Start, w.End, w.Word)
		}
		fmt.Println()
	}

	if len(tm.Sentences) > 0 {
		fmt.Println("SENTENCES")
		fmt.Println("---------")
		for i, s := range tm.Sentences {
			fmt.Printf("%2d. [%7.2fs - %7.2fs] %s\n", i+1, s.Start, s.End, s.Text)
		}
		fmt.Println()
	}

	fmt.Printf("Total: %d words, %d sentences\n", len(tm.Words), len(tm.Sentences))
	return nil
}
