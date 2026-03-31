package enrich

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os/exec"
	"strings"
	"time"
)

// YouTubeEnricher gathers YouTube video opportunities via the youtube CLI.
type YouTubeEnricher struct{}

// NewYouTubeEnricher creates a new YouTubeEnricher.
func NewYouTubeEnricher() *YouTubeEnricher {
	return &YouTubeEnricher{}
}

func (e *YouTubeEnricher) Platform() string { return "youtube" }

// youtubeScanItem mirrors the Candidate struct from the youtube CLI JSON output.
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

// Scan calls `youtube scan --json` and returns metadata-only opportunities.
func (e *YouTubeEnricher) Scan(ctx context.Context, limit int) ([]EnrichedOpportunity, error) {
	args := []string{"scan", "--json"}
	if limit > 0 {
		args = append(args, "--limit", fmt.Sprintf("%d", limit))
	}
	out, err := runCLI(ctx, "youtube", args...)
	if err != nil {
		if errors.Is(err, exec.ErrNotFound) {
			return nil, nil
		}
		return nil, fmt.Errorf("youtube scan: %w", err)
	}

	var items []youtubeScanItem
	if err := json.Unmarshal(out, &items); err != nil {
		return nil, fmt.Errorf("youtube scan: parsing output: %w", err)
	}

	result := make([]EnrichedOpportunity, 0, len(items))
	for _, item := range items {
		result = append(result, EnrichedOpportunity{
			ID:        item.ID,
			Platform:  "youtube",
			URL:       item.URL,
			Title:     item.Title,
			Body:      item.Body,
			Author:    item.Author,
			CreatedAt: item.CreatedAt,
			Comments:  []Comment{},
		})
	}
	return result, nil
}

// Enrich returns a YouTube video enriched with transcript if available.
// Transcript is fetched from YouTube's timedtext API (auto-captions, no key required).
func (e *YouTubeEnricher) Enrich(ctx context.Context, id string) (*EnrichedOpportunity, error) {
	if id == "" {
		return nil, fmt.Errorf("youtube enrich: video ID is required")
	}
	opp := &EnrichedOpportunity{
		ID:        id,
		Platform:  "youtube",
		URL:       fmt.Sprintf("https://www.youtube.com/watch?v=%s", id),
		Comments:  []Comment{},
		CreatedAt: time.Now(),
	}

	// Attempt transcript via YouTube timedtext API (auto-captions).
	transcript, err := fetchYouTubeTranscript(ctx, id)
	if err == nil && transcript != "" {
		opp.Transcript = transcript
	}

	return opp, nil
}

// fetchYouTubeTranscript tries to fetch auto-generated captions from YouTube's timedtext API.
// Returns an empty string without error if captions are unavailable.
func fetchYouTubeTranscript(ctx context.Context, videoID string) (string, error) {
	url := fmt.Sprintf("https://www.youtube.com/api/timedtext?v=%s&lang=en&fmt=json3", videoID)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return "", fmt.Errorf("building timedtext request: %w", err)
	}
	req.Header.Set("User-Agent", "nanika-engage/0.1")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("fetching timedtext: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return "", nil // captions not available for this video
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("reading timedtext response: %w", err)
	}
	if len(body) == 0 {
		return "", nil
	}

	return extractTimedTextTranscript(body), nil
}

// timedTextJSON3 is the structure of YouTube's json3 caption format.
type timedTextJSON3 struct {
	Events []struct {
		Segs []struct {
			Utf8 string `json:"utf8"`
		} `json:"segs"`
	} `json:"events"`
}

// extractTimedTextTranscript parses a json3 timedtext response into plain text.
func extractTimedTextTranscript(data []byte) string {
	var tt timedTextJSON3
	if err := json.Unmarshal(data, &tt); err != nil {
		return ""
	}
	var sb strings.Builder
	for _, ev := range tt.Events {
		for _, seg := range ev.Segs {
			sb.WriteString(seg.Utf8)
		}
	}
	return strings.TrimSpace(sb.String())
}
