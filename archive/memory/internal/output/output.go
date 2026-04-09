package output

import (
	"encoding/json"
	"os"
)

// JSON writes pretty JSON to stdout for machine-readable commands.
func JSON(v any) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}
