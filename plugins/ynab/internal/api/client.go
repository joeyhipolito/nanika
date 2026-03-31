// Package api provides the YNAB API client.
package api

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"time"
)

const (
	// BaseURL is the YNAB API base URL
	BaseURL = "https://api.youneedabudget.com/v1"

	// MaxRetries is the maximum number of retry attempts
	MaxRetries = 3

	// InitialBackoff is the initial backoff duration
	InitialBackoff = 1 * time.Second
)

// Client is the YNAB API client.
type Client struct {
	token            string
	baseURL          string
	httpClient       *http.Client
	defaultBudgetID  string
}


// NewClient creates a new YNAB API client.
// If token is empty, it will attempt to read from YNAB_ACCESS_TOKEN environment variable.
func NewClient(token string) (*Client, error) {
	if token == "" {
		token = os.Getenv("YNAB_ACCESS_TOKEN")
	}
	if token == "" {
		return nil, errors.New("YNAB_ACCESS_TOKEN is required")
	}

	return &Client{
		token:   token,
		baseURL: BaseURL,
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}, nil
}

// request performs an HTTP request with retry logic and rate limit handling.
func (c *Client) request(method, endpoint string, body io.Reader) ([]byte, error) {
	var lastErr error
	backoff := InitialBackoff

	for attempt := 0; attempt <= MaxRetries; attempt++ {
		if attempt > 0 {
			// Wait before retrying
			time.Sleep(backoff)
			backoff *= 2 // Exponential backoff
		}

		// Create request
		url := c.baseURL + endpoint
		req, err := http.NewRequest(method, url, body)
		if err != nil {
			return nil, fmt.Errorf("failed to create request: %w", err)
		}

		// Set headers
		req.Header.Set("Authorization", "Bearer "+c.token)
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("User-Agent", "Via-YNAB/2.0")

		// Execute request
		resp, err := c.httpClient.Do(req)
		if err != nil {
			lastErr = fmt.Errorf("request failed: %w", err)
			continue // Retry on network errors
		}

		// Read response body
		respBody, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil {
			lastErr = fmt.Errorf("failed to read response: %w", err)
			continue
		}

		// Handle rate limiting (429)
		if resp.StatusCode == http.StatusTooManyRequests {
			retryAfter := 60 // Default to 60 seconds
			if retryHeader := resp.Header.Get("Retry-After"); retryHeader != "" {
				if val, err := strconv.Atoi(retryHeader); err == nil {
					retryAfter = val
				}
			}
			lastErr = NewRateLimitError(retryAfter)
			// Wait for the specified retry-after period before retrying
			time.Sleep(time.Duration(retryAfter) * time.Second)
			continue
		}

		// Handle non-2xx status codes
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			// Try to parse error response
			var errorResp struct {
				Error struct {
					ID     string `json:"id"`
					Name   string `json:"name"`
					Detail string `json:"detail"`
				} `json:"error"`
			}

			var ynabErr *YNABError
			if err := json.Unmarshal(respBody, &errorResp); err == nil && errorResp.Error.Name != "" {
				ynabErr = &YNABError{
					Message:    errorResp.Error.Name,
					StatusCode: resp.StatusCode,
					ErrorID:    errorResp.Error.ID,
					Detail:     errorResp.Error.Detail,
				}
			} else {
				// Generic error if we can't parse the error response
				ynabErr = &YNABError{
					Message:    fmt.Sprintf("HTTP request failed: %s", resp.Status),
					StatusCode: resp.StatusCode,
				}
			}

			// Special handling for authentication errors (401)
			if resp.StatusCode == http.StatusUnauthorized {
				return nil, NewAuthError()
			}

			// Retry server errors (5xx) with exponential backoff
			if ynabErr.IsServerError() {
				lastErr = ynabErr
				continue
			}

			// Don't retry client errors (4xx except 429)
			return nil, ynabErr
		}

		// Success
		return respBody, nil
	}

	// All retries exhausted
	if lastErr != nil {
		return nil, fmt.Errorf("request failed after %d retries: %w", MaxRetries, lastErr)
	}
	return nil, fmt.Errorf("request failed after %d retries", MaxRetries)
}

// SetDefaultBudgetID sets the default budget ID (from config file).
func (c *Client) SetDefaultBudgetID(id string) {
	c.defaultBudgetID = id
}

// GetDefaultBudgetID lazily loads and returns the default budget ID.
func (c *Client) GetDefaultBudgetID() (string, error) {
	if c.defaultBudgetID != "" {
		return c.defaultBudgetID, nil
	}

	budgets, err := c.GetBudgets()
	if err != nil {
		return "", fmt.Errorf("failed to get budgets: %w", err)
	}

	if len(budgets) == 0 {
		return "", errors.New("no budgets found for this account")
	}

	// Use first budget as default
	c.defaultBudgetID = budgets[0].ID
	return c.defaultBudgetID, nil
}
