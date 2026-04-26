package index

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// EmbeddingClient generates text embeddings using the Gemini API.
// Ported from ~/via/archive/features/agents/internal/agents/embeddings.go.
type EmbeddingClient struct {
	apiKey     string
	model      string
	httpClient *http.Client
}

// geminiEmbedRequest is the request body for Gemini embedding API.
type geminiEmbedRequest struct {
	Model                string              `json:"model"`
	Content              geminiEmbedContent  `json:"content"`
	OutputDimensionality int                 `json:"outputDimensionality,omitempty"`
}

type geminiEmbedContent struct {
	Parts []geminiEmbedPart `json:"parts"`
}

type geminiEmbedPart struct {
	Text string `json:"text"`
}

// geminiEmbedResponse is the response from Gemini embedding API.
type geminiEmbedResponse struct {
	Embedding struct {
		Values []float32 `json:"values"`
	} `json:"embedding"`
	Error *geminiError `json:"error,omitempty"`
}

type geminiError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Status  string `json:"status"`
}

// NewEmbeddingClient creates a new Gemini embedding client.
func NewEmbeddingClient(apiKey string) *EmbeddingClient {
	return &EmbeddingClient{
		apiKey: apiKey,
		model:  "gemini-embedding-001", // flexible dimensions, free tier
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

// IsAvailable returns true if the API key is configured.
func (c *EmbeddingClient) IsAvailable() bool {
	return c.apiKey != ""
}

// Embed generates an embedding vector for the given text.
func (c *EmbeddingClient) Embed(ctx context.Context, text string) ([]float32, error) {
	if c.apiKey == "" {
		return nil, fmt.Errorf("Gemini API key not configured")
	}

	reqBody := geminiEmbedRequest{
		Model: fmt.Sprintf("models/%s", c.model),
		Content: geminiEmbedContent{
			Parts: []geminiEmbedPart{{Text: text}},
		},
		OutputDimensionality: 768,
	}

	jsonBody, err := json.Marshal(reqBody)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request: %w", err)
	}

	// GOTCHA: Gemini uses API key as query parameter, not Bearer token header
	url := fmt.Sprintf("https://generativelanguage.googleapis.com/v1beta/models/%s:embedContent?key=%s",
		c.model, c.apiKey)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(jsonBody))
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("request failed: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	var embedResp geminiEmbedResponse
	if err := json.Unmarshal(body, &embedResp); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w", err)
	}

	if embedResp.Error != nil {
		return nil, fmt.Errorf("API error %d: %s", embedResp.Error.Code, embedResp.Error.Message)
	}

	if len(embedResp.Embedding.Values) == 0 {
		return nil, fmt.Errorf("empty embedding returned")
	}

	return embedResp.Embedding.Values, nil
}

// EmbedBatch generates embeddings for multiple texts using the batch endpoint.
// More efficient than calling Embed multiple times.
func (c *EmbeddingClient) EmbedBatch(ctx context.Context, texts []string) ([][]float32, error) {
	if c.apiKey == "" {
		return nil, fmt.Errorf("Gemini API key not configured")
	}

	if len(texts) == 0 {
		return nil, nil
	}

	type batchRequest struct {
		Requests []geminiEmbedRequest `json:"requests"`
	}

	type batchResponse struct {
		Embeddings []struct {
			Values []float32 `json:"values"`
		} `json:"embeddings"`
		Error *geminiError `json:"error,omitempty"`
	}

	// Build batch request
	requests := make([]geminiEmbedRequest, len(texts))
	for i, text := range texts {
		requests[i] = geminiEmbedRequest{
			Model: fmt.Sprintf("models/%s", c.model),
			Content: geminiEmbedContent{
				Parts: []geminiEmbedPart{{Text: text}},
			},
			OutputDimensionality: 768,
		}
	}

	jsonBody, err := json.Marshal(batchRequest{Requests: requests})
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request: %w", err)
	}

	url := fmt.Sprintf("https://generativelanguage.googleapis.com/v1beta/models/%s:batchEmbedContents?key=%s",
		c.model, c.apiKey)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(jsonBody))
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("request failed: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	var batchResp batchResponse
	if err := json.Unmarshal(body, &batchResp); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w", err)
	}

	if batchResp.Error != nil {
		return nil, fmt.Errorf("API error %d: %s", batchResp.Error.Code, batchResp.Error.Message)
	}

	result := make([][]float32, len(batchResp.Embeddings))
	for i, emb := range batchResp.Embeddings {
		result[i] = emb.Values
	}

	return result, nil
}
