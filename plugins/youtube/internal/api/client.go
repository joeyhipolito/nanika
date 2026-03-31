package api

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

const (
	ytAPIBase = "https://www.googleapis.com/youtube/v3"

	// Quota unit costs per API call (YouTube Data API v3).
	ytQuotaSearchList     = 100
	ytQuotaCommentsInsert = 50
)

// Client calls the YouTube Data API v3.
// Quota usage is tracked in ~/.alluka/youtube.db (advisory — DB failure is non-fatal).
type Client struct {
	config   Config
	runUnits int // quota units consumed this run (in-memory)
	http     *http.Client
	db       *sql.DB // lazily opened; nil means quota DB writes unavailable
}

// NewClient creates a Client from ~/.alluka/youtube-config.json.
func NewClient() (*Client, error) {
	cfg, err := LoadConfig()
	if err != nil {
		return nil, err
	}
	return &Client{
		config: cfg,
		http:   &http.Client{Timeout: 30 * time.Second},
	}, nil
}

// NewClientFromConfig creates a Client from the given Config.
func NewClientFromConfig(cfg Config) *Client {
	return &Client{
		config: cfg,
		http:   &http.Client{Timeout: 30 * time.Second},
	}
}

// Config returns the client's configuration.
func (c *Client) Config() Config { return c.config }

// RunUnits returns the number of quota units consumed this run.
func (c *Client) RunUnits() int { return c.runUnits }

// openDB lazily opens the quota database.
func (c *Client) openDB() *sql.DB {
	if c.db != nil {
		return c.db
	}
	base, err := baseDir()
	if err != nil {
		return nil
	}
	if err := os.MkdirAll(base, 0700); err != nil {
		return nil
	}
	dbPath := filepath.Join(base, "youtube.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		return nil
	}
	db.SetMaxOpenConns(1)
	db.Exec("PRAGMA journal_mode=WAL")
	_, err = db.Exec(`
		CREATE TABLE IF NOT EXISTS quota_usage (
			id       INTEGER PRIMARY KEY AUTOINCREMENT,
			date     TEXT    NOT NULL,
			platform TEXT    NOT NULL,
			kind     TEXT    NOT NULL,
			count    INTEGER NOT NULL DEFAULT 0,
			UNIQUE(date, platform, kind)
		)`)
	if err != nil {
		_ = db.Close()
		return nil
	}
	c.db = db
	return db
}

// TodayQuota returns the total quota units consumed today.
// Returns 0 on any DB error (quota tracking is advisory).
func (c *Client) TodayQuota(ctx context.Context) int {
	db := c.openDB()
	if db == nil {
		return 0
	}
	today := time.Now().UTC().Format("2006-01-02")
	var count int
	_ = db.QueryRowContext(ctx,
		`SELECT COALESCE(SUM(count), 0) FROM quota_usage WHERE date=? AND platform='youtube' AND kind='units'`,
		today,
	).Scan(&count)
	return count
}

// consumeQuota checks budget and records usage. Returns false if budget exceeded.
func (c *Client) consumeQuota(ctx context.Context, units int) bool {
	if c.runUnits+units > c.config.Budget {
		return false
	}
	c.runUnits += units

	db := c.openDB()
	if db == nil {
		return true
	}
	today := time.Now().UTC().Format("2006-01-02")
	_, _ = db.ExecContext(ctx,
		`INSERT INTO quota_usage (date, platform, kind, count) VALUES (?, 'youtube', 'units', ?)
		 ON CONFLICT(date, platform, kind) DO UPDATE SET count = count + excluded.count`,
		today, units,
	)
	return true
}

// accessToken returns a valid Bearer token, refreshing if needed.
func (c *Client) accessToken() (string, error) {
	token, err := LoadToken()
	if err != nil {
		return "", fmt.Errorf("loading youtube token: %w", err)
	}
	if !token.IsExpired() {
		return token.AccessToken, nil
	}
	if c.config.ClientID == "" || c.config.ClientSecret == "" {
		return "", fmt.Errorf("token expired and no client_id/client_secret configured for refresh")
	}
	refreshed, err := RefreshToken(c.config.ClientID, c.config.ClientSecret, token.RefreshToken)
	if err != nil {
		return "", fmt.Errorf("refreshing youtube token: %w", err)
	}
	if err := SaveToken(refreshed); err != nil {
		fmt.Fprintf(os.Stderr, "youtube: warning: could not save refreshed token: %v\n", err)
	}
	return refreshed.AccessToken, nil
}

// Scan returns recent videos from configured channels as candidates.
// If opts.Topics is non-empty, also searches by topic (100 units each).
// Stops early if the quota budget is reached.
func (c *Client) Scan(ctx context.Context, opts ScanOpts) ([]Candidate, error) {
	if len(c.config.Channels) == 0 && len(opts.Topics) == 0 {
		return nil, fmt.Errorf("no channels configured and no topics provided")
	}

	limit := opts.Limit
	if limit <= 0 {
		limit = 20
	}

	var candidates []Candidate

	for _, channelID := range c.config.Channels {
		if len(candidates) >= limit {
			break
		}
		if !c.consumeQuota(ctx, ytQuotaSearchList) {
			fmt.Fprintf(os.Stderr, "youtube: quota budget (%d units) reached, stopping channel scan\n", c.config.Budget)
			break
		}
		videos, err := c.fetchChannelVideos(ctx, channelID, limit-len(candidates), opts.Since)
		if err != nil {
			fmt.Fprintf(os.Stderr, "youtube: fetching videos for channel %s: %v\n", channelID, err)
			continue
		}
		candidates = append(candidates, videos...)
	}

	for _, topic := range opts.Topics {
		if len(candidates) >= limit {
			break
		}
		if !c.consumeQuota(ctx, ytQuotaSearchList) {
			fmt.Fprintf(os.Stderr, "youtube: quota budget reached, skipping search for %q\n", topic)
			break
		}
		results, err := c.searchVideos(ctx, topic, limit-len(candidates))
		if err != nil {
			fmt.Fprintf(os.Stderr, "youtube: searching for %q: %v\n", topic, err)
			continue
		}
		candidates = append(candidates, results...)
	}

	return candidates, nil
}

// Comment posts a top-level comment on a video (50 units).
// videoID is the YouTube video ID (e.g. "dQw4w9WgXcQ").
func (c *Client) Comment(ctx context.Context, videoID, text string) (CommentResult, error) {
	if videoID == "" {
		return CommentResult{}, fmt.Errorf("video ID is required")
	}
	if text == "" {
		return CommentResult{}, fmt.Errorf("comment text is required")
	}
	if !c.consumeQuota(ctx, ytQuotaCommentsInsert) {
		return CommentResult{}, fmt.Errorf("youtube quota budget (%d units) exhausted", c.config.Budget)
	}

	bearer, err := c.accessToken()
	if err != nil {
		return CommentResult{}, fmt.Errorf("getting youtube access token: %w", err)
	}

	bodyJSON, err := json.Marshal(map[string]any{
		"snippet": map[string]any{
			"videoId": videoID,
			"topLevelComment": map[string]any{
				"snippet": map[string]any{
					"textOriginal": text,
				},
			},
		},
	})
	if err != nil {
		return CommentResult{}, fmt.Errorf("marshaling comment body: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost,
		ytAPIBase+"/commentThreads?part=snippet", bytes.NewReader(bodyJSON))
	if err != nil {
		return CommentResult{}, fmt.Errorf("building commentThreads.insert request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+bearer)
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.http.Do(req)
	if err != nil {
		return CommentResult{}, fmt.Errorf("calling commentThreads.insert: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return CommentResult{}, fmt.Errorf("reading comment response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return CommentResult{}, fmt.Errorf("commentThreads.insert failed (%d): %s", resp.StatusCode, respBody)
	}

	var result struct {
		ID      string `json:"id"`
		Snippet struct {
			TopLevelComment struct {
				ID      string `json:"id"`
				Snippet struct {
					PublishedAt string `json:"publishedAt"`
				} `json:"snippet"`
			} `json:"topLevelComment"`
		} `json:"snippet"`
	}
	if err := json.Unmarshal(respBody, &result); err != nil {
		return CommentResult{}, fmt.Errorf("parsing comment response: %w", err)
	}

	postedAt := time.Now()
	if result.Snippet.TopLevelComment.Snippet.PublishedAt != "" {
		if t, err := time.Parse(time.RFC3339, result.Snippet.TopLevelComment.Snippet.PublishedAt); err == nil {
			postedAt = t
		}
	}

	commentID := result.Snippet.TopLevelComment.ID
	if commentID == "" {
		commentID = result.ID
	}
	commentURL := fmt.Sprintf("https://www.youtube.com/watch?v=%s&lc=%s", videoID, commentID)

	return CommentResult{
		ID:        commentID,
		Platform:  "youtube",
		URL:       commentURL,
		CreatedAt: postedAt,
	}, nil
}

// Like likes a video via videos.rate (50 units).
func (c *Client) Like(ctx context.Context, videoID string) (CommentResult, error) {
	if videoID == "" {
		return CommentResult{}, fmt.Errorf("video ID is required")
	}
	if !c.consumeQuota(ctx, ytQuotaCommentsInsert) {
		return CommentResult{}, fmt.Errorf("youtube quota budget (%d units) exhausted", c.config.Budget)
	}

	bearer, err := c.accessToken()
	if err != nil {
		return CommentResult{}, fmt.Errorf("getting youtube access token: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost,
		ytAPIBase+"/videos/rate?id="+url.QueryEscape(videoID)+"&rating=like", nil)
	if err != nil {
		return CommentResult{}, fmt.Errorf("building videos.rate request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+bearer)

	resp, err := c.http.Do(req)
	if err != nil {
		return CommentResult{}, fmt.Errorf("calling videos.rate: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusNoContent && resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return CommentResult{}, fmt.Errorf("videos.rate failed (%d): %s", resp.StatusCode, body)
	}

	return CommentResult{
		Platform:  "youtube",
		URL:       "https://www.youtube.com/watch?v=" + videoID,
		CreatedAt: time.Now(),
	}, nil
}

// --- YouTube API response types ---

type ytError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

type ytSearchListResponse struct {
	Items []struct {
		ID struct {
			Kind    string `json:"kind"`
			VideoID string `json:"videoId"`
		} `json:"id"`
		Snippet struct {
			ChannelID   string `json:"channelId"`
			Title       string `json:"title"`
			Description string `json:"description"`
			PublishedAt string `json:"publishedAt"`
		} `json:"snippet"`
	} `json:"items"`
	Error *ytError `json:"error"`
}

// fetchChannelVideos calls search.list for a channel (quota already consumed by caller).
func (c *Client) fetchChannelVideos(ctx context.Context, channelID string, maxResults int, since time.Time) ([]Candidate, error) {
	if maxResults <= 0 {
		maxResults = 20
	}
	if maxResults > 10 {
		maxResults = 10
	}

	params := url.Values{
		"part":       {"snippet"},
		"channelId":  {channelID},
		"type":       {"video"},
		"order":      {"date"},
		"maxResults": {fmt.Sprintf("%d", maxResults)},
		"key":        {c.config.APIKey},
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet,
		ytAPIBase+"/search?"+params.Encode(), nil)
	if err != nil {
		return nil, fmt.Errorf("building search.list request: %w", err)
	}

	resp, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling search.list: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("reading search response: %w", err)
	}

	var result ytSearchListResponse
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("parsing search response: %w", err)
	}
	if result.Error != nil {
		return nil, fmt.Errorf("YouTube search error %d: %s", result.Error.Code, result.Error.Message)
	}

	return buildCandidates(result.Items, channelID, since), nil
}

// searchVideos calls search.list for a topic query (quota already consumed by caller).
func (c *Client) searchVideos(ctx context.Context, query string, maxResults int) ([]Candidate, error) {
	if maxResults <= 0 || maxResults > 50 {
		maxResults = 50
	}

	params := url.Values{
		"part":       {"snippet"},
		"q":          {query},
		"type":       {"video"},
		"maxResults": {fmt.Sprintf("%d", maxResults)},
		"key":        {c.config.APIKey},
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet,
		ytAPIBase+"/search?"+params.Encode(), nil)
	if err != nil {
		return nil, fmt.Errorf("building search.list request: %w", err)
	}

	resp, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling search.list: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("reading search response: %w", err)
	}

	var result ytSearchListResponse
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, fmt.Errorf("parsing search response: %w", err)
	}
	if result.Error != nil {
		return nil, fmt.Errorf("YouTube search API error %d: %s", result.Error.Code, result.Error.Message)
	}

	return buildCandidates(result.Items, "", time.Time{}), nil
}

// buildCandidates converts search result items into Candidate structs.
func buildCandidates(items []struct {
	ID struct {
		Kind    string `json:"kind"`
		VideoID string `json:"videoId"`
	} `json:"id"`
	Snippet struct {
		ChannelID   string `json:"channelId"`
		Title       string `json:"title"`
		Description string `json:"description"`
		PublishedAt string `json:"publishedAt"`
	} `json:"snippet"`
}, channelIDHint string, since time.Time) []Candidate {
	candidates := make([]Candidate, 0, len(items))
	for _, item := range items {
		videoID := item.ID.VideoID
		if videoID == "" {
			continue
		}

		var publishedAt time.Time
		if item.Snippet.PublishedAt != "" {
			publishedAt, _ = time.Parse(time.RFC3339, item.Snippet.PublishedAt)
		}
		if !since.IsZero() && !publishedAt.IsZero() && publishedAt.Before(since) {
			continue
		}

		channelID := item.Snippet.ChannelID
		if channelID == "" {
			channelID = channelIDHint
		}

		body := item.Snippet.Description
		if transcript := fetchTranscript(videoID); transcript != "" {
			body = transcript
		}

		candidates = append(candidates, Candidate{
			ID:        videoID,
			Platform:  "youtube",
			URL:       "https://www.youtube.com/watch?v=" + videoID,
			Title:     item.Snippet.Title,
			Body:      body,
			Author:    channelID,
			CreatedAt: publishedAt,
			Meta: map[string]string{
				"channel_id": channelID,
				"video_id":   videoID,
			},
		})
	}
	return candidates
}

// fetchTranscript shells out to the youtube-transcript Python script to get
// video captions. Returns the first ~3000 chars of transcript, or empty string
// on any failure (not all videos have captions).
func fetchTranscript(videoID string) string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	script := filepath.Join(home, "nanika", ".claude", "skills", "youtube-transcript", "scripts", "get_transcript.py")

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, "uv", "run", "--script", script, videoID)
	out, err := cmd.Output()
	if err != nil {
		return ""
	}

	transcript := strings.TrimSpace(string(out))
	if len(transcript) > 3000 {
		transcript = transcript[:3000] + "\n[transcript truncated]"
	}
	return transcript
}
