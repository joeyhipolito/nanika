// Package gemini provides a text generation client for the Gemini API.
// Follows the same patterns as obsidian's internal/index/embeddings.go.
package gemini

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// Client generates text using the Gemini generateContent API.
type Client struct {
	apiKey     string
	model      string
	httpClient *http.Client
}

// --- Request / response types ---

type generateRequest struct {
	Contents         []generateContent  `json:"contents"`
	GenerationConfig *generationConfig  `json:"generationConfig,omitempty"`
}

type generateContent struct {
	Parts []generatePart `json:"parts"`
}

type generatePart struct {
	Text string `json:"text"`
}

type generationConfig struct {
	ResponseMIMEType string  `json:"responseMimeType,omitempty"`
	Temperature      float64 `json:"temperature,omitempty"`
	MaxOutputTokens  int     `json:"maxOutputTokens,omitempty"`
}

type generateResponse struct {
	Candidates []struct {
		Content      generateContent `json:"content"`
		FinishReason string          `json:"finishReason"`
	} `json:"candidates"`
	Error *geminiError `json:"error,omitempty"`
}

type geminiError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Status  string `json:"status"`
}

// --- Constructor ---

// NewClient creates a Gemini text generation client.
// apiKey is the Google AI Studio API key; pass "" if unavailable
// (IsAvailable() will return false and all calls will error).
func NewClient(apiKey string) *Client {
	return &Client{
		apiKey: apiKey,
		model:  "gemini-2.0-flash",
		httpClient: &http.Client{
			Timeout: 60 * time.Second,
		},
	}
}

// NewClientWithModel creates a client with a specific model.
func NewClientWithModel(apiKey, model string) *Client {
	c := NewClient(apiKey)
	c.model = model
	return c
}

// --- Methods ---

// IsAvailable returns true if the API key is configured.
func (c *Client) IsAvailable() bool {
	return c.apiKey != ""
}

// Generate sends a prompt to Gemini and returns the text response.
func (c *Client) Generate(ctx context.Context, prompt string) (string, error) {
	if c.apiKey == "" {
		return "", fmt.Errorf("Gemini API key not configured")
	}
	if prompt == "" {
		return "", fmt.Errorf("prompt is required")
	}

	reqBody := generateRequest{
		Contents: []generateContent{
			{Parts: []generatePart{{Text: prompt}}},
		},
	}

	return c.doGenerate(ctx, reqBody)
}

// GenerateJSON sends a prompt and asks Gemini to respond with JSON.
// The raw JSON string is returned; the caller unmarshals into their type.
func (c *Client) GenerateJSON(ctx context.Context, prompt string) (string, error) {
	if c.apiKey == "" {
		return "", fmt.Errorf("Gemini API key not configured")
	}
	if prompt == "" {
		return "", fmt.Errorf("prompt is required")
	}

	reqBody := generateRequest{
		Contents: []generateContent{
			{Parts: []generatePart{{Text: prompt}}},
		},
		GenerationConfig: &generationConfig{
			ResponseMIMEType: "application/json",
		},
	}

	raw, err := c.doGenerate(ctx, reqBody)
	if err != nil {
		return "", err
	}

	// Strip markdown fences if the model wrapped the JSON anyway.
	raw = stripMarkdownFences(raw)

	return raw, nil
}

// GenerateInto sends a prompt, requests JSON, and unmarshals the result into v.
// v must be a pointer to the target type.
func (c *Client) GenerateInto(ctx context.Context, prompt string, v any) error {
	raw, err := c.GenerateJSON(ctx, prompt)
	if err != nil {
		return err
	}
	if err := json.Unmarshal([]byte(raw), v); err != nil {
		return fmt.Errorf("parsing Gemini JSON response: %w", err)
	}
	return nil
}

// --- Internal helpers ---

func (c *Client) doGenerate(ctx context.Context, reqBody generateRequest) (string, error) {
	jsonBody, err := json.Marshal(reqBody)
	if err != nil {
		return "", fmt.Errorf("marshaling request: %w", err)
	}

	// GOTCHA: Gemini uses the API key as a query parameter, not a Bearer header.
	url := fmt.Sprintf(
		"https://generativelanguage.googleapis.com/v1beta/models/%s:generateContent?key=%s",
		c.model, c.apiKey,
	)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(jsonBody))
	if err != nil {
		return "", fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("request failed: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("reading response body: %w", err)
	}

	var genResp generateResponse
	if err := json.Unmarshal(body, &genResp); err != nil {
		return "", fmt.Errorf("parsing response: %w", err)
	}

	if genResp.Error != nil {
		return "", fmt.Errorf("API error %d (%s): %s",
			genResp.Error.Code, genResp.Error.Status, genResp.Error.Message)
	}

	if len(genResp.Candidates) == 0 {
		return "", fmt.Errorf("no candidates in response")
	}
	if len(genResp.Candidates[0].Content.Parts) == 0 {
		return "", fmt.Errorf("no parts in response candidate")
	}

	return genResp.Candidates[0].Content.Parts[0].Text, nil
}

// stripMarkdownFences removes ```json ... ``` or ``` ... ``` wrappers
// that the model may add even when responseMimeType is application/json.
func stripMarkdownFences(s string) string {
	s = strings.TrimSpace(s)
	if strings.HasPrefix(s, "```") {
		// Remove opening fence line
		if idx := strings.Index(s, "\n"); idx >= 0 {
			s = s[idx+1:]
		}
		// Remove closing fence
		if idx := strings.LastIndex(s, "```"); idx >= 0 {
			s = s[:idx]
		}
		s = strings.TrimSpace(s)
	}
	return s
}
