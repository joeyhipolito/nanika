// Package api implements the YouTube Data API v3 client for the youtube CLI.
package api

import "time"

// Config is loaded from ~/.alluka/youtube-config.json.
type Config struct {
	APIKey       string   `json:"api_key"`
	ClientID     string   `json:"client_id"`
	ClientSecret string   `json:"client_secret"`
	Channels     []string `json:"channels"` // YouTube channel IDs to scan
	Budget       int      `json:"budget"`   // max quota units per run (default 10000)
}

// Token holds OAuth2 credentials for the YouTube Data API.
type Token struct {
	AccessToken  string    `json:"access_token"`
	RefreshToken string    `json:"refresh_token"`
	Expiry       time.Time `json:"expiry"`
	TokenType    string    `json:"token_type"`
}

// IsExpired returns true if the access token has expired (with 60-second buffer).
func (t *Token) IsExpired() bool {
	return time.Now().Add(60 * time.Second).After(t.Expiry)
}

// Candidate is a video that may be engaged with (commented on or liked).
type Candidate struct {
	ID        string            `json:"id"`
	Platform  string            `json:"platform"`
	URL       string            `json:"url"`
	Title     string            `json:"title"`
	Body      string            `json:"body"`
	Author    string            `json:"author"`
	CreatedAt time.Time         `json:"created_at"`
	Meta      map[string]string `json:"meta,omitempty"`
}

// ScanOpts controls how a channel scan is performed.
type ScanOpts struct {
	Topics []string
	Limit  int
	Since  time.Time
}

// CommentResult is the outcome of a successful comment or like action.
type CommentResult struct {
	ID        string    `json:"id"`
	Platform  string    `json:"platform"`
	URL       string    `json:"url"`
	CreatedAt time.Time `json:"created_at"`
}

// DoctorCheck is one named health check.
type DoctorCheck struct {
	Name    string `json:"name"`
	Status  string `json:"status"`  // "ok", "warn", "fail"
	Message string `json:"message"`
}

// DoctorResult reports the health of the YouTube CLI configuration.
type DoctorResult struct {
	Platform string        `json:"platform"`
	OK       bool          `json:"all_ok"`
	Checks   []DoctorCheck `json:"checks"`
	Summary  string        `json:"summary"`
}
