// review-scanner — code-review blocker scanner.
// Discovers review files produced by staff-code-reviewer workers and surfaces
// each blocker as a Finding for the nen dashboard.
//
// Usage: review-scanner --workspace <id>
// Output: Envelope JSON on stdout.
package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
)

const (
	scannerName         = "review-scanner"
	abilityCodeReview   = "code-review"
	categoryBlocker     = "review-blocker"
)

func main() {
	var workspaceID string
	flag.StringVar(&workspaceID, "workspace", "", "workspace ID to scan (required)")
	flag.Parse()

	if workspaceID == "" {
		fmt.Fprintln(os.Stderr, "review-scanner: --workspace is required")
		os.Exit(1)
	}

	findings, warnings, err := runScan(workspaceID)
	if err != nil {
		warnings = append(warnings, fmt.Sprintf("review-scanner: %v", err))
	}

	envelope := scan.NewEnvelope(scannerName, abilityCodeReview, findings, warnings)
	out, err := json.Marshal(envelope)
	if err != nil {
		fmt.Fprintf(os.Stderr, "review-scanner: marshalling envelope: %v\n", err)
		os.Exit(1)
	}
	fmt.Println(string(out))
}

func runScan(workspaceID string) ([]scan.Finding, []string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, nil, fmt.Errorf("resolving home dir: %w", err)
	}

	pattern := filepath.Join(home, ".alluka", "workspaces", workspaceID, "workers", "staff-code-reviewer-*", "review*.md")
	matches, err := filepath.Glob(pattern)
	if err != nil {
		return nil, nil, fmt.Errorf("globbing review files: %w", err)
	}

	var findings []scan.Finding
	var warnings []string
	now := time.Now().UTC()

	for _, reviewFile := range matches {
		data, err := os.ReadFile(reviewFile)
		if err != nil {
			warnings = append(warnings, fmt.Sprintf("reading %s: %v", reviewFile, err))
			continue
		}

		blockers := scan.ParseReviewBlockers(string(data))
		for _, b := range blockers {
			id := findingID(workspaceID, reviewFile, b.Title)
			scope := scan.Scope{Kind: "workspace", Value: workspaceID}
			desc := b.Description
			if b.File != "" {
				desc = fmt.Sprintf("%s (see %s", desc, b.File)
				if b.LineStart > 0 {
					if b.LineEnd > b.LineStart {
						desc = fmt.Sprintf("%s:%d-%d", desc, b.LineStart, b.LineEnd)
					} else {
						desc = fmt.Sprintf("%s:%d", desc, b.LineStart)
					}
				}
				desc += ")"
			}
			findings = append(findings, scan.Finding{
				ID:          id,
				Ability:     abilityCodeReview,
				Category:    categoryBlocker,
				Severity:    scan.SeverityHigh,
				Title:       b.Title,
				Description: desc,
				Scope:       scope,
				Source:      reviewFile,
				FoundAt:     now,
				Evidence: []scan.Evidence{
					{
						Kind:       "review-file",
						Raw:        b.Title,
						Source:     reviewFile,
						CapturedAt: now,
					},
				},
			})
		}
	}

	return findings, warnings, nil
}

// findingID produces a deterministic, stable ID for a review blocker.
// ID = "rev_" + hex(sha256(workspaceID + "|" + filePath + "|" + title)[:8])
func findingID(workspaceID, filePath, title string) string {
	h := sha256.Sum256([]byte(workspaceID + "|" + filePath + "|" + title))
	return "rev_" + hex.EncodeToString(h[:8])
}
