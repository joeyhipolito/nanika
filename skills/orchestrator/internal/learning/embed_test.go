package learning

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestEmbed_HTTPStatusError(t *testing.T) {
	tests := []struct {
		name        string
		statusCode  int
		body        string
		wantErr     bool
		errContains string
	}{
		{
			name:        "401 unauthorized after key rotation",
			statusCode:  http.StatusUnauthorized,
			body:        `{"error":{"code":401,"message":"API key not valid. Please pass a valid API key."}}`,
			wantErr:     true,
			errContains: "HTTP 401",
		},
		{
			name:        "429 rate limited",
			statusCode:  http.StatusTooManyRequests,
			body:        `{"error":{"code":429,"message":"RESOURCE_EXHAUSTED"}}`,
			wantErr:     true,
			errContains: "HTTP 429",
		},
		{
			name:        "500 server error with non-JSON body",
			statusCode:  http.StatusInternalServerError,
			body:        "internal server error",
			wantErr:     true,
			errContains: "HTTP 500",
		},
		{
			name:       "200 ok with valid embedding",
			statusCode: http.StatusOK,
			body:       `{"embedding":{"values":[0.1,0.2,0.3]}}`,
			wantErr:    false,
		},
		{
			name:        "200 ok with empty values",
			statusCode:  http.StatusOK,
			body:        `{"embedding":{"values":[]}}`,
			wantErr:     true,
			errContains: "empty embedding",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(tt.statusCode)
				fmt.Fprint(w, tt.body)
			}))
			defer srv.Close()

			e := &Embedder{
				apiKey:     "test-key",
				model:      "gemini-embedding-001",
				httpClient: srv.Client(),
				baseURL:    srv.URL,
			}

			got, err := e.Embed(context.Background(), "hello world")
			if tt.wantErr {
				if err == nil {
					t.Fatalf("Embed() = %v, want error", got)
				}
				if tt.errContains != "" && !strings.Contains(err.Error(), tt.errContains) {
					t.Errorf("Embed() error = %q, want it to contain %q", err.Error(), tt.errContains)
				}
				return
			}
			if err != nil {
				t.Fatalf("Embed() unexpected error: %v", err)
			}
			if len(got) == 0 {
				t.Error("Embed() returned empty slice, want non-empty")
			}
		})
	}
}

func TestEmbed_NilEmbedder(t *testing.T) {
	var e *Embedder
	_, err := e.Embed(context.Background(), "test")
	if err == nil {
		t.Fatal("Embed on nil Embedder should return error")
	}
}

func TestEmbedBatch_HardenedStatusHandling(t *testing.T) {
	tests := []struct {
		name        string
		statusCode  int
		body        string
		header      map[string]string
		wantErr     bool
		wantHTTP    int
		wantRetryAt time.Duration
	}{
		{
			name:        "429 with Retry-After is surfaced as HTTPStatusError",
			statusCode:  http.StatusTooManyRequests,
			body:        `{"error":{"code":429,"message":"RESOURCE_EXHAUSTED"}}`,
			header:      map[string]string{"Retry-After": "3"},
			wantErr:     true,
			wantHTTP:    429,
			wantRetryAt: 3 * time.Second,
		},
		{
			name:       "500 server error is HTTPStatusError",
			statusCode: http.StatusInternalServerError,
			body:       "boom",
			wantErr:    true,
			wantHTTP:   500,
		},
		{
			name:       "200 with mismatched embedding count returns plain error",
			statusCode: http.StatusOK,
			body:       `{"embeddings":[{"values":[0.1]}]}`, // only 1 returned for 2 inputs
			wantErr:    true,
		},
		{
			name:       "200 with two valid embeddings",
			statusCode: http.StatusOK,
			body:       `{"embeddings":[{"values":[0.1,0.2]},{"values":[0.3,0.4]}]}`,
			wantErr:    false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				for k, v := range tt.header {
					w.Header().Set(k, v)
				}
				w.WriteHeader(tt.statusCode)
				fmt.Fprint(w, tt.body)
			}))
			defer srv.Close()

			e := &Embedder{
				apiKey:     "test-key",
				model:      "gemini-embedding-001",
				httpClient: srv.Client(),
				baseURL:    srv.URL,
			}

			vecs, err := e.EmbedBatch(context.Background(), []string{"a", "b"})
			if !tt.wantErr {
				if err != nil {
					t.Fatalf("EmbedBatch unexpected error: %v", err)
				}
				if len(vecs) != 2 {
					t.Fatalf("EmbedBatch returned %d vectors, want 2", len(vecs))
				}
				return
			}
			if err == nil {
				t.Fatalf("EmbedBatch expected error, got %v", vecs)
			}
			if tt.wantHTTP > 0 {
				var hse *HTTPStatusError
				if !errors.As(err, &hse) {
					t.Fatalf("expected *HTTPStatusError, got %T (%v)", err, err)
				}
				if hse.Status != tt.wantHTTP {
					t.Errorf("HTTPStatusError.Status = %d, want %d", hse.Status, tt.wantHTTP)
				}
				if hse.RetryAfter != tt.wantRetryAt {
					t.Errorf("HTTPStatusError.RetryAfter = %s, want %s", hse.RetryAfter, tt.wantRetryAt)
				}
			}
		})
	}
}

// TestInsert_WritePathGuardRejectsNilEmbedding asserts that DB.Insert refuses
// to write a row when an embedder is configured but cannot produce a vector
// (network error). Without the guard, the row would land with embedding =
// NULL — which is exactly the failure mode the backfill subcommand exists to
// repair. The legacy nil-embedder path is preserved (verified separately).
func TestInsert_WritePathGuardRejectsNilEmbedding(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
		fmt.Fprint(w, "boom")
	}))
	defer srv.Close()

	embedder := &Embedder{
		apiKey:     "test-key",
		model:      "gemini-embedding-001",
		httpClient: srv.Client(),
		baseURL:    srv.URL,
	}

	db := newTestDB(t)
	l := Learning{
		ID:        "guarded-row",
		Type:      TypeInsight,
		Content:   "would have been a NULL embedding row",
		Domain:    "dev",
		CreatedAt: time.Now(),
	}

	err := db.Insert(context.Background(), l, embedder)
	if err == nil {
		t.Fatal("Insert with failing embedder should return an error, got nil")
	}

	// The row must NOT be present.
	var n int
	if err := db.db.QueryRow(`SELECT COUNT(*) FROM learnings WHERE id = ?`, l.ID).Scan(&n); err != nil {
		t.Fatalf("count: %v", err)
	}
	if n != 0 {
		t.Errorf("guarded row was written despite embedder failure (count=%d)", n)
	}

	// Sanity: the legacy nil-embedder path still allows storing rows without
	// an embedding (used by docs ingestion + test fixtures).
	if err := db.Insert(context.Background(), l, nil); err != nil {
		t.Fatalf("Insert with nil embedder must still work for legacy callers: %v", err)
	}
}
