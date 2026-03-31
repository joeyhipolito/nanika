// Package doctor provides diagnostic check rendering for the substack CLI.
package doctor

import (
	"encoding/json"
	"fmt"
	"io"
	"strings"
)

// Check represents a single diagnostic check result.
type Check struct {
	Name    string `json:"name"`
	Status  string `json:"status"`
	Message string `json:"message"`
}

// Output is the structured output for the doctor command.
type Output struct {
	Checks  []Check `json:"checks"`
	Summary string  `json:"summary"`
	AllOK   bool    `json:"all_ok"`
}

// Render writes the doctor output either as JSON or as a formatted text table.
func Render(w io.Writer, checks []Check, allOK bool, jsonOutput bool, title string, failOnError bool) error {
	summary := "all checks passed"
	if !allOK {
		summary = "some checks failed"
	}

	if jsonOutput {
		out := Output{Checks: checks, Summary: summary, AllOK: allOK}
		enc := json.NewEncoder(w)
		enc.SetIndent("", "  ")
		if err := enc.Encode(out); err != nil {
			return err
		}
		if failOnError && !allOK {
			return fmt.Errorf("doctor checks failed")
		}
		return nil
	}

	fmt.Fprintln(w, title)
	fmt.Fprintln(w, strings.Repeat("=", len(title)))
	for _, check := range checks {
		icon := "  OK"
		switch check.Status {
		case "warn":
			icon = "WARN"
		case "fail":
			icon = "FAIL"
		}
		fmt.Fprintf(w, "  [%4s] %-22s %s\n", icon, check.Name+":", check.Message)
	}
	fmt.Fprintf(w, "\nSummary: %s\n", summary)

	if !allOK {
		return fmt.Errorf("doctor checks failed")
	}
	return nil
}
