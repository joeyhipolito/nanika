package learning

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

// EmbeddingDimensions is the size of gemini-embedding-001 vectors.
const EmbeddingDimensions = 3072

// Embedder generates text embeddings via the Gemini API.
type Embedder struct {
	apiKey     string
	model      string
	httpClient *http.Client
}

type geminiEmbedRequest struct {
	Model   string             `json:"model"`
	Content geminiEmbedContent `json:"content"`
}

type geminiEmbedContent struct {
	Parts []geminiEmbedPart `json:"parts"`
}

type geminiEmbedPart struct {
	Text string `json:"text"`
}

type geminiEmbedResponse struct {
	Embedding struct {
		Values []float32 `json:"values"`
	} `json:"embedding"`
	Error *struct {
		Code    int    `json:"code"`
		Message string `json:"message"`
	} `json:"error,omitempty"`
}

// NewEmbedder creates a new Gemini embedding client.
// Returns nil if apiKey is empty.
func NewEmbedder(apiKey string) *Embedder {
	if apiKey == "" {
		return nil
	}
	return &Embedder{
		apiKey: apiKey,
		model:  "gemini-embedding-001",
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

// Embed generates an embedding vector for the given text.
func (e *Embedder) Embed(ctx context.Context, text string) ([]float32, error) {
	if e == nil {
		return nil, fmt.Errorf("embedder not configured")
	}

	reqBody := geminiEmbedRequest{
		Model:   fmt.Sprintf("models/%s", e.model),
		Content: geminiEmbedContent{Parts: []geminiEmbedPart{{Text: text}}},
	}

	jsonBody, err := json.Marshal(reqBody)
	if err != nil {
		return nil, err
	}

	url := fmt.Sprintf("https://generativelanguage.googleapis.com/v1beta/models/%s:embedContent",
		e.model)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(jsonBody))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("x-goog-api-key", e.apiKey)

	resp, err := e.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("embedding request failed: %w", err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	var embedResp geminiEmbedResponse
	if err := json.Unmarshal(body, &embedResp); err != nil {
		return nil, err
	}

	if embedResp.Error != nil {
		return nil, fmt.Errorf("API error %d: %s", embedResp.Error.Code, embedResp.Error.Message)
	}

	if len(embedResp.Embedding.Values) == 0 {
		return nil, fmt.Errorf("empty embedding returned")
	}

	return embedResp.Embedding.Values, nil
}

// EmbedBatch generates embeddings for multiple texts.
func (e *Embedder) EmbedBatch(ctx context.Context, texts []string) ([][]float32, error) {
	if e == nil {
		return nil, fmt.Errorf("embedder not configured")
	}
	if len(texts) == 0 {
		return nil, nil
	}

	type batchReq struct {
		Requests []geminiEmbedRequest `json:"requests"`
	}
	type batchResp struct {
		Embeddings []struct {
			Values []float32 `json:"values"`
		} `json:"embeddings"`
	}

	requests := make([]geminiEmbedRequest, len(texts))
	for i, text := range texts {
		requests[i] = geminiEmbedRequest{
			Model:   fmt.Sprintf("models/%s", e.model),
			Content: geminiEmbedContent{Parts: []geminiEmbedPart{{Text: text}}},
		}
	}

	jsonBody, err := json.Marshal(batchReq{Requests: requests})
	if err != nil {
		return nil, err
	}

	url := fmt.Sprintf("https://generativelanguage.googleapis.com/v1beta/models/%s:batchEmbedContents",
		e.model)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(jsonBody))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("x-goog-api-key", e.apiKey)

	resp, err := e.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	var br batchResp
	if err := json.Unmarshal(body, &br); err != nil {
		return nil, err
	}

	result := make([][]float32, len(br.Embeddings))
	for i, emb := range br.Embeddings {
		result[i] = emb.Values
	}
	return result, nil
}

// CosineSimilarity computes cosine similarity between two vectors.
func CosineSimilarity(a, b []float32) float64 {
	if len(a) != len(b) || len(a) == 0 {
		return 0
	}
	var dot, normA, normB float64
	for i := range a {
		ai, bi := float64(a[i]), float64(b[i])
		dot += ai * bi
		normA += ai * ai
		normB += bi * bi
	}
	if normA == 0 || normB == 0 {
		return 0
	}
	return dot / (math.Sqrt(normA) * math.Sqrt(normB))
}

// EncodeEmbedding converts float32 slice to bytes (little-endian).
func EncodeEmbedding(v []float32) []byte {
	if v == nil {
		return nil
	}
	buf := make([]byte, len(v)*4)
	for i, f := range v {
		binary.LittleEndian.PutUint32(buf[i*4:], math.Float32bits(f))
	}
	return buf
}

// DecodeEmbedding converts bytes back to float32 slice.
func DecodeEmbedding(b []byte) []float32 {
	if len(b) == 0 || len(b)%4 != 0 {
		return nil
	}
	v := make([]float32, len(b)/4)
	for i := range v {
		v[i] = math.Float32frombits(binary.LittleEndian.Uint32(b[i*4:]))
	}
	return v
}

// LoadAPIKey loads the Gemini API key from env or config files.
func LoadAPIKey() string {
	if key := os.Getenv("GEMINI_API_KEY"); key != "" {
		return strings.TrimSpace(key)
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}

	// Try orchestrator config dir and ~/.obsidian/config
	paths := []string{filepath.Join(home, ".obsidian", "config")}
	if base, err := config.Dir(); err == nil {
		paths = append([]string{filepath.Join(base, "config")}, paths...)
	}
	for _, path := range paths {
		if key := loadKeyFromFile(path); key != "" {
			return key
		}
	}
	return ""
}

func loadKeyFromFile(path string) string {
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	for _, line := range strings.Split(string(data), "\n") {
		line = strings.TrimSpace(line)
		if strings.HasPrefix(line, "gemini_apikey=") {
			return strings.TrimSpace(strings.TrimPrefix(line, "gemini_apikey="))
		}
		if strings.HasPrefix(line, "gemini_apikey:") {
			return strings.TrimSpace(strings.TrimPrefix(line, "gemini_apikey:"))
		}
	}
	return ""
}
