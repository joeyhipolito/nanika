package api

import (
	"crypto/tls"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

const (
	baseURL   = "https://www.reddit.com"
	userAgent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
)

// RedditClient handles requests to Reddit's web API using cookie-based auth.
type RedditClient struct {
	BaseURL       string
	RedditSession string
	CSRFToken     string
	HTTPClient    *http.Client
	LastReqAt     time.Time
	MinDelay      time.Duration
}

// NewRedditClient creates a new Reddit API client.
func NewRedditClient(redditSession, csrfToken string) *RedditClient {
	// Force HTTP/1.1 — Reddit's bot detection blocks Go's HTTP/2 TLS fingerprint.
	transport := &http.Transport{
		TLSClientConfig: &tls.Config{
			NextProtos: []string{"http/1.1"},
		},
	}
	return &RedditClient{
		BaseURL:       baseURL,
		RedditSession: redditSession,
		CSRFToken:     csrfToken,
		HTTPClient: &http.Client{
			Timeout:   30 * time.Second,
			Transport: transport,
		},
		MinDelay: 2 * time.Second,
	}
}

// doGet executes a GET request to a Reddit .json endpoint with rate limiting.
func (c *RedditClient) doGet(path string) ([]byte, error) {
	c.rateLimit()

	req, err := http.NewRequest("GET", c.BaseURL+path, nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}

	c.setHeaders(req)

	c.LastReqAt = time.Now()
	resp, err := c.HTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("executing request: %w", err)
	}
	defer resp.Body.Close()

	return c.handleResponse(resp)
}

// doPost executes a POST request with form-encoded body.
func (c *RedditClient) doPost(path string, form url.Values) ([]byte, error) {
	c.rateLimit()

	req, err := http.NewRequest("POST", c.BaseURL+path, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}

	c.setHeaders(req)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	c.LastReqAt = time.Now()
	resp, err := c.HTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("executing request: %w", err)
	}
	defer resp.Body.Close()

	return c.handleResponse(resp)
}

func (c *RedditClient) setHeaders(req *http.Request) {
	req.Header.Set("Cookie", fmt.Sprintf("reddit_session=%s; csrf_token=%s", c.RedditSession, c.CSRFToken))
	req.Header.Set("X-CSRF-Token", c.CSRFToken)
	req.Header.Set("User-Agent", userAgent)
	req.Header.Set("X-Requested-With", "XMLHttpRequest")
}

func (c *RedditClient) rateLimit() {
	if !c.LastReqAt.IsZero() {
		elapsed := time.Since(c.LastReqAt)
		if elapsed < c.MinDelay {
			time.Sleep(c.MinDelay - elapsed)
		}
	}
}

func (c *RedditClient) handleResponse(resp *http.Response) ([]byte, error) {
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("reading response body: %w", err)
	}

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("cookies expired or invalid. Run 'reddit configure cookies' to refresh")
	}
	if resp.StatusCode == http.StatusTooManyRequests {
		return nil, fmt.Errorf("rate limited by Reddit. Try again in a few minutes")
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		snippet := string(body)
		if len(snippet) > 200 {
			snippet = snippet[:200] + "..."
		}
		return nil, fmt.Errorf("Reddit API error (HTTP %d): %s", resp.StatusCode, snippet)
	}

	return body, nil
}

// TestAuth performs a lightweight request to verify cookie auth is working.
// Returns the username if successful.
func (c *RedditClient) TestAuth() (string, error) {
	data, err := c.doGet("/api/me.json")
	if err != nil {
		return "", err
	}

	var result struct {
		Data struct {
			Name string `json:"name"`
		} `json:"data"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return "", fmt.Errorf("parsing auth response: %w", err)
	}

	if result.Data.Name == "" {
		return "", fmt.Errorf("not authenticated — no username returned. Run 'reddit configure cookies'")
	}

	return result.Data.Name, nil
}
