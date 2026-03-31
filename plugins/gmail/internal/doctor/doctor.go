// Package doctor provides a check runner framework for CLI diagnostic commands.
// Inlined from github.com/joeyhipolito/publishing-shared/doctor to remove the external dependency.
package doctor

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
)

// Check represents a single diagnostic check result.
type Check struct {
	Name    string `json:"name"`
	Status  string `json:"status"`  // "ok", "warn", "fail"
	Message string `json:"message"`
}

// Output is the structured output for the doctor command.
type Output struct {
	Checks  []Check `json:"checks"`
	Summary string  `json:"summary"`
	AllOK   bool    `json:"all_ok"`
}

// Render writes the doctor output either as JSON or as a formatted text table.
// The title is the CLI name shown in text mode (e.g. "Gmail Doctor").
// If failOnError is true, also returns an error when checks fail.
func Render(w io.Writer, checks []Check, allOK bool, jsonOutput bool, title string, failOnError bool) error {
	summary := "all checks passed"
	if !allOK {
		summary = "some checks failed"
	}

	if jsonOutput {
		out := Output{
			Checks:  checks,
			Summary: summary,
			AllOK:   allOK,
		}
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
	fmt.Fprintln(w, repeatChar('=', len(title)))
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

// BinaryInPath returns a Check that verifies `name` is found in PATH.
// A missing binary is a warn, not a fail — local builds won't be in PATH.
func BinaryInPath(name string) Check {
	path, err := exec.LookPath(name)
	if err != nil {
		return Check{
			Name:    "Binary",
			Status:  "warn",
			Message: name + " not found in PATH (running from local build?)",
		}
	}
	return Check{Name: "Binary", Status: "ok", Message: path}
}

// ConfigPermissions returns a Check that verifies configPath has 0600 permissions.
// If the file does not exist, it returns a skipped check (Status == "").
func ConfigPermissions(configPath string) Check {
	info, err := os.Stat(configPath)
	if err != nil {
		if os.IsNotExist(err) {
			return Check{} // skip — caller already reported missing config
		}
		return Check{
			Name:    "Config permissions",
			Status:  "fail",
			Message: fmt.Sprintf("cannot read permissions: %v", err),
		}
	}
	perms := info.Mode().Perm()
	if perms != 0600 {
		return Check{
			Name:    "Config permissions",
			Status:  "warn",
			Message: fmt.Sprintf("%o (should be 600). Fix: chmod 600 %s", perms, configPath),
		}
	}
	return Check{Name: "Config permissions", Status: "ok", Message: "600 (secure)"}
}

func repeatChar(c byte, n int) string {
	b := make([]byte, n)
	for i := range b {
		b[i] = c
	}
	return string(b)
}
