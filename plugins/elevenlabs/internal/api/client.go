// Package api provides types and HTTP client for the ElevenLabs REST API.
package api

import (
	"context"
	"fmt"
	"net/http"
	"time"
)

const baseURL = "https://api.elevenlabs.io"

// Client is an HTTP client authenticated with an ElevenLabs API key.
type Client struct {
	apiKey     string
	httpClient *http.Client
}

// NewClient creates a Client with the given API key.
func NewClient(apiKey string) *Client {
	return &Client{
		apiKey: apiKey,
		httpClient: &http.Client{
			Timeout: 120 * time.Second,
		},
	}
}

// newRequest creates a GET/POST request pre-populated with auth headers.
func (c *Client) newRequest(ctx context.Context, method, path string) (*http.Request, error) {
	req, err := http.NewRequestWithContext(ctx, method, baseURL+path, nil)
	if err != nil {
		return nil, fmt.Errorf("building request for %s %s: %w", method, path, err)
	}
	req.Header.Set("xi-api-key", c.apiKey)
	req.Header.Set("Accept", "application/json")
	return req, nil
}
