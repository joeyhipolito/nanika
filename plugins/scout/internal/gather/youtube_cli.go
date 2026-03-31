package gather

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

// YouTubeCLIGatherer fetches videos via the youtube CLI (youtube scan --json).
// Requires the youtube CLI to be installed and configured with an API key.
// Source name is "youtube-cli" to distinguish from the RSS-based "youtube" source.
type YouTubeCLIGatherer struct{}

// NewYouTubeCLIGatherer creates a new YouTube CLI gatherer.
func NewYouTubeCLIGatherer() *YouTubeCLIGatherer {
	return &YouTubeCLIGatherer{}
}

// Name returns the canonical source identifier.
func (g *YouTubeCLIGatherer) Name() string { return "youtube-cli" }

// youtubeScanItem mirrors the JSON shape emitted by `youtube scan --json`.
type youtubeScanItem struct {
	ID        string            `json:"id"`
	Platform  string            `json:"platform"`
	URL       string            `json:"url"`
	Title     string            `json:"title"`
	Body      string            `json:"body"`
	Author    string            `json:"author"`
	CreatedAt time.Time         `json:"created_at"`
	Meta      map[string]string `json:"meta,omitempty"`
}

// Gather runs `youtube scan --json [--topics ...]` and maps results to IntelItems.
// Returns nil items (not an error) if the youtube CLI is not installed.
func (g *YouTubeCLIGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	path, err := exec.LookPath("youtube")
	if err != nil {
		fmt.Fprintf(os.Stderr, "    Warning: youtube-cli: youtube CLI not installed, skipping\n")
		return nil, nil
	}

	args := []string{"scan", "--json"}
	if len(searchTerms) > 0 {
		args = append(args, "--topics", strings.Join(searchTerms, ","))
	}

	cmd := exec.CommandContext(ctx, path, args...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("youtube-cli: running youtube scan --json: %w (stderr: %s)", err, stderr.String())
	}

	var scanItems []youtubeScanItem
	if err := json.Unmarshal(stdout.Bytes(), &scanItems); err != nil {
		return nil, fmt.Errorf("youtube-cli: parsing youtube scan output: %w", err)
	}

	return mapYouTubeScanItems(scanItems), nil
}

// mapYouTubeScanItems converts youtube CLI scan results to IntelItems.
func mapYouTubeScanItems(scanItems []youtubeScanItem) []IntelItem {
	seen := make(map[string]bool)
	items := make([]IntelItem, 0, len(scanItems))

	for _, s := range scanItems {
		id := s.ID
		if id == "" {
			id = generateID(s.URL)
		}
		if seen[id] {
			continue
		}
		seen[id] = true

		ts := s.CreatedAt
		if ts.IsZero() {
			ts = time.Now().UTC()
		}

		items = append(items, IntelItem{
			ID:        id,
			Title:     cleanText(s.Title),
			Content:   cleanText(s.Body),
			SourceURL: s.URL,
			Author:    cleanText(s.Author),
			Timestamp: ts,
			Tags:      []string{"youtube-cli"},
		})
	}

	return items
}
