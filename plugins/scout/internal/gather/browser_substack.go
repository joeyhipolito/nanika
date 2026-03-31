package gather

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"time"
)

// SubstackBrowserGatherer fetches the Substack inbox via the substack CLI.
// Requires the substack CLI to be installed and configured with a valid session cookie.
type SubstackBrowserGatherer struct{}

// NewSubstackBrowserGatherer creates a new Substack inbox gatherer.
func NewSubstackBrowserGatherer() *SubstackBrowserGatherer {
	return &SubstackBrowserGatherer{}
}

func (g *SubstackBrowserGatherer) Name() string { return "substack-browser" }

// substackScoutItem mirrors the JSON shape emitted by `substack feed --scout`.
type substackScoutItem struct {
	ID         string    `json:"id"`
	Title      string    `json:"title"`
	Content    string    `json:"content"`
	SourceURL  string    `json:"source_url"`
	Author     string    `json:"author"`
	Timestamp  time.Time `json:"timestamp"`
	Tags       []string  `json:"tags"`
	Engagement int       `json:"engagement,omitempty"`
}

// Gather runs `substack feed --scout` and maps results to IntelItems.
// Returns nil items (not an error) if the substack CLI is not installed.
func (g *SubstackBrowserGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	path, err := exec.LookPath("substack")
	if err != nil {
		fmt.Fprintf(os.Stderr, "    Warning: substack-browser: substack CLI not installed, skipping\n")
		return nil, nil
	}

	cmd := exec.CommandContext(ctx, path, "feed", "--scout")
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("substack-browser: running substack feed --scout: %w (stderr: %s)", err, stderr.String())
	}

	var scoutItems []substackScoutItem
	if err := json.Unmarshal(stdout.Bytes(), &scoutItems); err != nil {
		return nil, fmt.Errorf("substack-browser: parsing substack feed output: %w", err)
	}

	seen := make(map[string]bool)
	var items []IntelItem

	for _, si := range scoutItems {
		if seen[si.ID] {
			continue
		}
		seen[si.ID] = true

		items = append(items, IntelItem{
			ID:         si.ID,
			Title:      si.Title,
			Content:    si.Content,
			SourceURL:  si.SourceURL,
			Author:     si.Author,
			Timestamp:  si.Timestamp,
			Tags:       si.Tags,
			Engagement: si.Engagement,
		})
	}

	if len(searchTerms) > 0 {
		items = filterByTerms(items, searchTerms)
	}

	return items, nil
}
