package gemini

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// rewriteTransport redirects all HTTP requests to a test server by replacing
// the scheme and host, leaving the path and query intact.
type rewriteTransport struct {
	URL string
}

func (rt *rewriteTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	req.URL.Scheme = "http"
	req.URL.Host = rt.URL[len("http://"):]
	return http.DefaultTransport.RoundTrip(req)
}

// mockResponse builds a valid Gemini generateContent success response.
func mockResponse(text string) []byte {
	resp := generateResponse{
		Candidates: []struct {
			Content      generateContent `json:"content"`
			FinishReason string          `json:"finishReason"`
		}{
			{
				Content:      generateContent{Parts: []generatePart{{Text: text}}},
				FinishReason: "STOP",
			},
		},
	}
	data, _ := json.Marshal(resp)
	return data
}

// mockErrorResponse builds a valid Gemini API error response.
func mockErrorResponse(code int, status, message string) []byte {
	resp := generateResponse{
		Error: &geminiError{Code: code, Status: status, Message: message},
	}
	data, _ := json.Marshal(resp)
	return data
}

// clientWithTestServer returns a Client that routes requests to ts instead of
// the real Gemini endpoint.
func clientWithTestServer(apiKey string, ts *httptest.Server) *Client {
	c := NewClient(apiKey)
	c.httpClient = ts.Client()
	c.httpClient.Transport = &rewriteTransport{URL: ts.URL}
	return c
}

// ─── IsAvailable ─────────────────────────────────────────────────────────────

func TestIsAvailable_EmptyKey(t *testing.T) {
	c := NewClient("")
	if c.IsAvailable() {
		t.Error("IsAvailable() should be false when API key is empty")
	}
}

func TestIsAvailable_NonEmptyKey(t *testing.T) {
	c := NewClient("any-key")
	if !c.IsAvailable() {
		t.Error("IsAvailable() should be true when API key is non-empty")
	}
}

// ─── NewClientWithModel ───────────────────────────────────────────────────────

func TestNewClient_DefaultModel(t *testing.T) {
	c := NewClient("key")
	if c.model != "gemini-2.0-flash" {
		t.Errorf("default model: want 'gemini-2.0-flash', got %q", c.model)
	}
}

func TestNewClientWithModel_SetsModel(t *testing.T) {
	c := NewClientWithModel("key", "gemini-pro")
	if c.model != "gemini-pro" {
		t.Errorf("model: want 'gemini-pro', got %q", c.model)
	}
}

func TestNewClientWithModel_SetsKey(t *testing.T) {
	c := NewClientWithModel("my-key", "gemini-pro")
	if c.apiKey != "my-key" {
		t.Errorf("apiKey: want 'my-key', got %q", c.apiKey)
	}
}

// ─── Generate guard conditions ────────────────────────────────────────────────

func TestGenerate_NoAPIKey_ReturnsError(t *testing.T) {
	c := NewClient("")
	_, err := c.Generate(context.Background(), "hello")
	if err == nil {
		t.Fatal("expected error when API key is empty")
	}
	if !strings.Contains(err.Error(), "not configured") {
		t.Errorf("expected 'not configured' in error, got: %v", err)
	}
}

func TestGenerate_EmptyPrompt_ReturnsError(t *testing.T) {
	c := NewClient("some-key")
	_, err := c.Generate(context.Background(), "")
	if err == nil {
		t.Fatal("expected error for empty prompt")
	}
	if !strings.Contains(err.Error(), "prompt is required") {
		t.Errorf("expected 'prompt is required' in error, got: %v", err)
	}
}

func TestGenerateJSON_NoAPIKey_ReturnsError(t *testing.T) {
	c := NewClient("")
	_, err := c.GenerateJSON(context.Background(), "hello")
	if err == nil {
		t.Fatal("expected error when API key is empty")
	}
}

func TestGenerateJSON_EmptyPrompt_ReturnsError(t *testing.T) {
	c := NewClient("some-key")
	_, err := c.GenerateJSON(context.Background(), "")
	if err == nil {
		t.Fatal("expected error for empty prompt")
	}
}

// ─── Generate happy path ──────────────────────────────────────────────────────

func TestGenerate_HappyPath(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse("Hello from Gemini!"))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	result, err := c.Generate(context.Background(), "say hello")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if result != "Hello from Gemini!" {
		t.Errorf("want 'Hello from Gemini!', got %q", result)
	}
}

func TestGenerate_RequestIncludesContentType(t *testing.T) {
	var gotContentType string
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotContentType = r.Header.Get("Content-Type")
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse("ok"))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	if _, err := c.Generate(context.Background(), "prompt"); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if gotContentType != "application/json" {
		t.Errorf("expected Content-Type: application/json, got %q", gotContentType)
	}
}

// ─── Generate error responses ─────────────────────────────────────────────────

func TestGenerate_APIError_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockErrorResponse(400, "INVALID_ARGUMENT", "API key not valid"))
	}))
	defer ts.Close()

	c := clientWithTestServer("bad-key", ts)
	_, err := c.Generate(context.Background(), "hello")
	if err == nil {
		t.Fatal("expected error for API error response")
	}
	if !strings.Contains(err.Error(), "API error") {
		t.Errorf("expected 'API error' in message, got: %v", err)
	}
	if !strings.Contains(err.Error(), "API key not valid") {
		t.Errorf("expected API error message, got: %v", err)
	}
}

func TestGenerate_NoCandidates_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"candidates": []}`))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	_, err := c.Generate(context.Background(), "hello")
	if err == nil {
		t.Fatal("expected error for empty candidates list")
	}
	if !strings.Contains(err.Error(), "no candidates") {
		t.Errorf("expected 'no candidates' in error, got: %v", err)
	}
}

func TestGenerate_NoParts_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		// Candidate with empty parts slice
		w.Write([]byte(`{"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}]}`))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	_, err := c.Generate(context.Background(), "hello")
	if err == nil {
		t.Fatal("expected error when candidate has no parts")
	}
	if !strings.Contains(err.Error(), "no parts") {
		t.Errorf("expected 'no parts' in error, got: %v", err)
	}
}

func TestGenerate_InvalidJSON_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(`{invalid json`))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	_, err := c.Generate(context.Background(), "hello")
	if err == nil {
		t.Fatal("expected error for invalid JSON response")
	}
}

func TestGenerate_HTTP500_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
		// Empty JSON body → no candidates
		w.Write([]byte(`{}`))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	_, err := c.Generate(context.Background(), "hello")
	if err == nil {
		t.Fatal("expected error for 500 response with empty candidates")
	}
}

// ─── GenerateJSON ─────────────────────────────────────────────────────────────

func TestGenerateJSON_StripsMarkdownFences(t *testing.T) {
	jsonWithFences := "```json\n{\"key\": \"value\"}\n```"
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse(jsonWithFences))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	result, err := c.GenerateJSON(context.Background(), "return json")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.HasPrefix(result, "```") {
		t.Error("markdown fences should be stripped from GenerateJSON result")
	}
	// Must be parseable JSON after stripping
	var parsed map[string]interface{}
	if err := json.Unmarshal([]byte(result), &parsed); err != nil {
		t.Errorf("result should be valid JSON after fence stripping, got: %q (err: %v)", result, err)
	}
}

func TestGenerateJSON_CleanJSON_NotModified(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse(`{"suggestions": ["one", "two"]}`))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	result, err := c.GenerateJSON(context.Background(), "return json")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	var parsed map[string]interface{}
	if err := json.Unmarshal([]byte(result), &parsed); err != nil {
		t.Errorf("clean JSON should remain valid, got: %q", result)
	}
}

func TestGenerateJSON_SetsResponseMIMEType(t *testing.T) {
	var reqBody generateRequest
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewDecoder(r.Body).Decode(&reqBody)
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse(`{}`))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	if _, err := c.GenerateJSON(context.Background(), "return json"); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if reqBody.GenerationConfig == nil {
		t.Fatal("expected GenerationConfig to be set")
	}
	if reqBody.GenerationConfig.ResponseMIMEType != "application/json" {
		t.Errorf("expected ResponseMIMEType 'application/json', got %q",
			reqBody.GenerationConfig.ResponseMIMEType)
	}
}

// ─── GenerateInto ─────────────────────────────────────────────────────────────

func TestGenerateInto_UnmarshalsIntoStruct(t *testing.T) {
	type testResult struct {
		Message string `json:"message"`
		Count   int    `json:"count"`
	}

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse(`{"message": "hello", "count": 42}`))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	var result testResult
	if err := c.GenerateInto(context.Background(), "return json", &result); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if result.Message != "hello" {
		t.Errorf("message: want 'hello', got %q", result.Message)
	}
	if result.Count != 42 {
		t.Errorf("count: want 42, got %d", result.Count)
	}
}

func TestGenerateInto_InvalidJSON_ReturnsError(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse("this is not json at all"))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	var result map[string]interface{}
	err := c.GenerateInto(context.Background(), "return json", &result)
	if err == nil {
		t.Fatal("expected error when Gemini returns non-JSON text")
	}
	if !strings.Contains(err.Error(), "parsing Gemini JSON response") {
		t.Errorf("expected 'parsing Gemini JSON response' in error, got: %v", err)
	}
}

func TestGenerateInto_FencedJSONUnmarshalsSucessfully(t *testing.T) {
	// Even when the model wraps JSON in fences, GenerateInto should succeed.
	type payload struct {
		Name string `json:"name"`
	}

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse("```json\n{\"name\": \"gemini\"}\n```"))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)
	var result payload
	if err := c.GenerateInto(context.Background(), "return json", &result); err != nil {
		t.Fatalf("unexpected error with fenced JSON: %v", err)
	}
	if result.Name != "gemini" {
		t.Errorf("name: want 'gemini', got %q", result.Name)
	}
}

// ─── stripMarkdownFences ──────────────────────────────────────────────────────

func TestStripMarkdownFences(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "no fences — unchanged",
			input: `{"key": "value"}`,
			want:  `{"key": "value"}`,
		},
		{
			name:  "json fence",
			input: "```json\n{\"key\": \"value\"}\n```",
			want:  `{"key": "value"}`,
		},
		{
			name:  "plain fence",
			input: "```\n{\"key\": \"value\"}\n```",
			want:  `{"key": "value"}`,
		},
		{
			name:  "fenced with surrounding whitespace",
			input: "  ```json\n{\"key\": \"value\"}\n```  ",
			want:  `{"key": "value"}`,
		},
		{
			name:  "empty string",
			input: "",
			want:  "",
		},
		{
			name:  "fence with no content",
			input: "```json\n```",
			want:  "",
		},
		{
			name:  "plain text no fence",
			input: "Hello world",
			want:  "Hello world",
		},
		{
			name:  "multiline JSON in fence",
			input: "```json\n{\n  \"a\": 1,\n  \"b\": 2\n}\n```",
			want:  "{\n  \"a\": 1,\n  \"b\": 2\n}",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := stripMarkdownFences(tt.input)
			if got != tt.want {
				t.Errorf("stripMarkdownFences(%q)\n  want: %q\n   got: %q", tt.input, tt.want, got)
			}
		})
	}
}

// ─── Concurrency ─────────────────────────────────────────────────────────────

func TestGenerate_ConcurrentCalls(t *testing.T) {
	// Verify the client is safe to use from multiple goroutines (no shared
	// mutable state beyond httpClient which is already goroutine-safe).
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(mockResponse("ok"))
	}))
	defer ts.Close()

	c := clientWithTestServer("test-key", ts)

	done := make(chan error, 5)
	for i := 0; i < 5; i++ {
		go func() {
			_, err := c.Generate(context.Background(), "hello")
			done <- err
		}()
	}
	for i := 0; i < 5; i++ {
		if err := <-done; err != nil {
			t.Errorf("concurrent Generate call failed: %v", err)
		}
	}
}
