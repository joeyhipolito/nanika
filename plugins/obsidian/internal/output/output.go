// Package output provides JSON output helpers for the Obsidian CLI.
// Every command supports a --json flag; this package standardizes the encoding.
package output

import (
	"encoding/json"
	"os"
)

// JSON encodes v as pretty-printed JSON to stdout.
// Uses 2-space indentation, matching the ynab-cli pattern.
func JSON(v any) error {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(v)
}
