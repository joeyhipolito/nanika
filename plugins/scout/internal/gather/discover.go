package gather

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// discoverItem is the JSON structure returned by discover-scrape.sh.
type discoverItem struct {
	Title   string `json:"title"`
	URL     string `json:"url"`
	Source  string `json:"source"`
	Snippet string `json:"snippet"`
}

// DiscoverGatherer scrapes Google Discover feed via agent-browser.
type DiscoverGatherer struct{}

// NewDiscoverGatherer creates a new Discover gatherer.
func NewDiscoverGatherer() *DiscoverGatherer {
	return &DiscoverGatherer{}
}

func (g *DiscoverGatherer) Name() string { return "discover" }

func (g *DiscoverGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	// Check if agent-browser is available
	if _, err := exec.LookPath("agent-browser"); err != nil {
		fmt.Fprintln(os.Stderr, "    Warning: agent-browser not found, skipping Discover gathering")
		return nil, nil
	}

	scriptPath := findDiscoverScript()
	if scriptPath == "" {
		fmt.Fprintln(os.Stderr, "    Warning: discover-scrape.sh not found, skipping Discover gathering")
		return nil, nil
	}

	cmd := exec.Command("bash", scriptPath)
	output, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("discover scrape failed: %w", err)
	}

	var raw []discoverItem
	if err := json.Unmarshal(output, &raw); err != nil {
		return nil, fmt.Errorf("failed to parse discover output: %w", err)
	}

	now := time.Now().UTC()
	seen := make(map[string]bool)
	var items []IntelItem

	for _, d := range raw {
		if d.URL == "" || d.Title == "" {
			continue
		}
		id := generateID(d.URL)
		if seen[id] {
			continue
		}
		seen[id] = true

		items = append(items, IntelItem{
			ID:        id,
			Title:     cleanText(d.Title),
			Content:   cleanText(d.Snippet),
			SourceURL: d.URL,
			Author:    d.Source,
			Timestamp: now,
			Tags:      []string{"discover"},
		})
	}

	if len(searchTerms) > 0 {
		items = filterByTerms(items, searchTerms)
	}

	return items, nil
}

// findDiscoverScript locates discover-scrape.sh using common locations.
func findDiscoverScript() string {
	candidates := []string{}

	// Next to the binary
	if exe, err := os.Executable(); err == nil {
		candidates = append(candidates, filepath.Join(filepath.Dir(exe), "scripts", "discover-scrape.sh"))
	}

	// Known install location
	home, _ := os.UserHomeDir()
	if home != "" {
		candidates = append(candidates,
			filepath.Join(home, "skills", "scout", "scripts", "discover-scrape.sh"),
		)
	}

	// Source tree (for development)
	if wd, err := os.Getwd(); err == nil {
		candidates = append(candidates, filepath.Join(wd, "scripts", "discover-scrape.sh"))
	}

	for _, p := range candidates {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	return ""
}
