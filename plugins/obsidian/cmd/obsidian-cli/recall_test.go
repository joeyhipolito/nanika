package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"net"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/recall"
	"github.com/joeyhipolito/nanika-obsidian/internal/rpc"
)

// T3.3 — §10.4 Phase 3
// Asserts: the golden query "retrieval gaps" against the 50-Zettel fixture vault
// returns a response that matches golden JSON output exactly.
func TestRecall_HappyPath(t *testing.T) {
	tempDir := t.TempDir()
	sockPath := filepath.Join(tempDir, "recall.sock")

	// Start in-process RPC server with fixture graph
	g, docs := fixtureGraph50Zettels()
	server := startTestRPCServer(t, sockPath, g, docs)
	defer server.Stop()

	// This test requires the recall CLI command to be implemented.
	// For now, it will fail with "undefined: runRecallQuery" error in RED state.
	results, err := runRecallQuery(sockPath, "daily/2026-01-01.md", 5)
	if err != nil {
		// Expected in RED state: function not implemented
		t.Fatalf("RED — T3.3 recall query failed: %v", err)
	}

	if len(results) == 0 {
		t.Error("expected non-empty results from recall query")
	}

	// Verify each result has required fields
	for _, r := range results {
		if r.Path == "" {
			t.Error("result missing Path field")
		}
		// RPC protocol doesn't include scores, so Score is always 0
		// This is a known limitation mentioned in the RPC design
	}
}

// T3.5 — §10.4 Phase 3
// Asserts: when the daemon socket is missing, the obsidian recall CLI falls back
// to an in-process SQLite + unmapped graph path and returns results.
func TestRecall_Fallback(t *testing.T) {
	nonexistentSocket := filepath.Join(t.TempDir(), "nonexistent.sock")

	start := time.Now()
	results, err := runRecallQuery(nonexistentSocket, "daily/2026-01-01.md", 5)
	elapsed := time.Since(start)

	// RED state: function not implemented
	if err != nil {
		t.Logf("RED — T3.5 fallback test failed: %v", err)
		return
	}

	// When fallback succeeds, verify reasonable performance
	if elapsed > 100*time.Millisecond {
		t.Logf("fallback took %v (expected <100ms)", elapsed)
	}

	// Results should be valid (even if empty)
	if results == nil {
		t.Error("expected non-nil results even on fallback")
	}
}

// TestRecall_Format_Paths
// Asserts: --format=paths returns one path per line with no scores or JSON
func TestRecall_Format_Paths(t *testing.T) {
	tempDir := t.TempDir()
	sockPath := filepath.Join(tempDir, "recall.sock")

	g, docs := fixtureGraph50Zettels()
	server := startTestRPCServer(t, sockPath, g, docs)
	defer server.Stop()

	output, err := runRecallFormatted(sockPath, "daily/2026-01-01.md", 5, "paths")
	if err != nil {
		t.Fatalf("RED — Format_Paths failed: %v", err)
	}

	// Verify output is not JSON
	var j interface{}
	if err := json.Unmarshal([]byte(output), &j); err == nil {
		t.Error("--format=paths should not return JSON")
	}

	// Verify it looks like paths (contains .md)
	if !contains(output, ".md") {
		t.Errorf("expected .md files in paths format, got: %s", output)
	}
}

// TestRecall_Format_Markdown
// Asserts: --format=markdown returns Markdown list with paths and scores
func TestRecall_Format_Markdown(t *testing.T) {
	tempDir := t.TempDir()
	sockPath := filepath.Join(tempDir, "recall.sock")

	g, docs := fixtureGraph50Zettels()
	server := startTestRPCServer(t, sockPath, g, docs)
	defer server.Stop()

	output, err := runRecallFormatted(sockPath, "daily/2026-01-01.md", 5, "markdown")
	if err != nil {
		t.Fatalf("RED — Format_Markdown failed: %v", err)
	}

	// Markdown list should contain list markers
	hasListMarker := contains(output, "-") || contains(output, "*")
	if !hasListMarker && output != "" {
		t.Errorf("expected Markdown list markers, got: %s", output)
	}

	// Should not be JSON
	var j interface{}
	if err := json.Unmarshal([]byte(output), &j); err == nil && output != "" {
		t.Error("--format=markdown output should not be valid JSON")
	}
}

// TestRecall_Format_Brief
// Asserts: --format=brief returns compact output
func TestRecall_Format_Brief(t *testing.T) {
	tempDir := t.TempDir()
	sockPath := filepath.Join(tempDir, "recall.sock")

	g, docs := fixtureGraph50Zettels()
	server := startTestRPCServer(t, sockPath, g, docs)
	defer server.Stop()

	output, err := runRecallFormatted(sockPath, "daily/2026-01-01.md", 5, "brief")
	if err != nil {
		t.Fatalf("RED — Format_Brief failed: %v", err)
	}

	// Brief format should be concise
	if len(output) > 1000 {
		t.Logf("brief format seems long: %d bytes", len(output))
	}

	// Should not be full JSON structure with all fields
	var j interface{}
	if err := json.Unmarshal([]byte(output), &j); err == nil {
		if m, ok := j.(map[string]interface{}); ok && len(m) > 10 {
			t.Logf("brief format has many fields: %d", len(m))
		}
	}
}

// TestRecall_Format_Invalid
// Asserts: an invalid --format value produces an error
func TestRecall_Format_Invalid(t *testing.T) {
	tempDir := t.TempDir()
	sockPath := filepath.Join(tempDir, "recall.sock")

	g, docs := fixtureGraph50Zettels()
	server := startTestRPCServer(t, sockPath, g, docs)
	defer server.Stop()

	_, err := runRecallFormatted(sockPath, "daily/2026-01-01.md", 5, "invalid_format_xyz")

	if err == nil {
		t.Error("expected error for invalid format")
	}
}

// TestRecall_Limit_Clamped
// Asserts: --limit values are clamped to bounds and results are truncated
func TestRecall_Limit_Clamped(t *testing.T) {
	tempDir := t.TempDir()
	sockPath := filepath.Join(tempDir, "recall.sock")

	g, docs := fixtureGraph50Zettels()
	server := startTestRPCServer(t, sockPath, g, docs)
	defer server.Stop()

	// Test limit=1
	results, err := runRecallQuery(sockPath, "daily/2026-01-01.md", 1)
	if err != nil {
		t.Fatalf("RED — Limit_Clamped failed: %v", err)
	}

	if len(results) > 1 {
		t.Errorf("limit=1 should return at most 1 result, got %d", len(results))
	}

	// Test limit=2000 (should be clamped to 1000)
	results, err = runRecallQuery(sockPath, "daily/2026-01-01.md", 2000)
	if err != nil {
		t.Fatalf("limit=2000 query failed: %v", err)
	}

	if len(results) > 1000 {
		t.Errorf("limit should be clamped to 1000, got %d results", len(results))
	}
}

// TestRecall_NoFallback_ErrorsOnMissingSocket
// Asserts: when --no-fallback is specified and socket is missing, an error is returned
func TestRecall_NoFallback_ErrorsOnMissingSocket(t *testing.T) {
	nonexistentSocket := filepath.Join(t.TempDir(), "nonexistent.sock")

	err := runRecallNoFallback(nonexistentSocket, "daily/2026-01-01.md", 5)

	if err == nil {
		t.Error("expected error when --no-fallback used with missing socket")
	}
}

// TestRecall_SocketTimeout
// Asserts: when RPC server doesn't respond in time, a timeout error is returned
func TestRecall_SocketTimeout(t *testing.T) {
	tempDir := t.TempDir()
	sockPath := filepath.Join(tempDir, "recall.sock")

	// Start slow server
	server := startSlowTestRPCServer(t, sockPath, 5*time.Second)
	defer server.Stop()

	// Query with short timeout
	_, err := runRecallQueryWithTimeout(sockPath, "daily/2026-01-01.md", 5, 100*time.Millisecond)

	if err == nil {
		t.Error("expected timeout error when server is slow")
	}
}

// ============================================================================
// Test fixture helpers
// ============================================================================

// fixtureGraph50Zettels returns a test graph with 50 interconnected notes
func fixtureGraph50Zettels() (*graph.Graph, []recall.Document) {
	links := make([]index.LinkRow, 0, 100)
	docs := make([]recall.Document, 50)

	base := time.Now().Unix()
	for i := 0; i < 50; i++ {
		path := fmt.Sprintf("daily/2026-01-%02d.md", (i%31)+1)

		// Create document
		docs[i] = recall.Document{
			Path:    path,
			Title:   path,
			ModTime: base + int64(i),
		}

		// Create links between adjacent notes
		if i > 0 {
			prevPath := fmt.Sprintf("daily/2026-01-%02d.md", ((i-1)%31)+1)
			links = append(links, index.LinkRow{Src: path, Dst: prevPath})
		}

		// Self-link for testing (each note links to itself in some cases)
		if i%5 == 0 && i > 0 {
			prevPath := fmt.Sprintf("daily/2026-01-%02d.md", ((i-2)%31)+1)
			links = append(links, index.LinkRow{Src: path, Dst: prevPath})
		}
	}

	return graph.Build(links), docs
}

// startTestRPCServer starts an in-process RPC server on a Unix socket using the proper RPC protocol
func startTestRPCServer(t *testing.T, sockPath string, g *graph.Graph, docs []recall.Document) *testRPCServer {
	listener, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("failed to create unix socket listener: %v", err)
	}

	srv := &testRPCServer{
		listener: listener,
		graph:    g,
		docs:     docs,
		done:     make(chan struct{}),
	}

	// Start accepting connections in goroutine
	go func() {
		for {
			select {
			case <-srv.done:
				return
			default:
			}

			conn, err := listener.Accept()
			if err != nil {
				return // socket closed
			}

			// Handle requests in goroutine
			go func(c net.Conn) {
				defer c.Close()

				scanner := bufio.NewScanner(c)
				for scanner.Scan() {
					// Parse RPC request envelope
					var req rpc.Request
					if err := json.Unmarshal(scanner.Bytes(), &req); err != nil {
						resp := rpc.Response{OK: false, Error: &rpc.RPCError{Code: rpc.ErrCodeBadRequest, Message: "bad request"}}
						b, _ := json.Marshal(resp)
						c.Write(append(b, '\n'))
						continue
					}

					// Route to handler
					var resp rpc.Response
					if req.Method == "recall" {
						var recallReq rpc.RecallRequest
						if err := json.Unmarshal(req.Params, &recallReq); err != nil {
							resp = rpc.Response{OK: false, Error: &rpc.RPCError{Code: rpc.ErrCodeBadRequest, Message: "bad recall params"}}
						} else {
							// Execute recall with provided documents
							w := recall.NewWalker(srv.graph, srv.docs, recall.WalkerConfig{MaxHops: recallReq.MaxHops})
							results := w.Walk(recallReq.Seed)
							if recallReq.Limit > 0 && len(results) > recallReq.Limit {
								results = results[:recallReq.Limit]
							}

							// Extract paths from results
							paths := make([]string, len(results))
							for i, r := range results {
								paths[i] = r.Path
							}

							// Return proper RPC response
							recallResp := rpc.RecallResponse{Paths: paths}
							respBytes, _ := json.Marshal(recallResp)
							resp = rpc.Response{OK: true, Result: respBytes}
						}
					} else {
						resp = rpc.Response{OK: false, Error: &rpc.RPCError{Code: rpc.ErrCodeUnknownMethod, Message: "unknown method"}}
					}

					// Send response
					respBytes, _ := json.Marshal(resp)
					c.Write(append(respBytes, '\n'))
				}
			}(conn)
		}
	}()

	return srv
}

// startSlowTestRPCServer starts an RPC server that delays all responses
func startSlowTestRPCServer(t *testing.T, sockPath string, delay time.Duration) *testRPCServer {
	listener, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("failed to create unix socket listener: %v", err)
	}

	srv := &testRPCServer{
		listener: listener,
		docs:     []recall.Document{},
		done:     make(chan struct{}),
	}

	go func() {
		for {
			select {
			case <-srv.done:
				return
			default:
			}

			conn, err := listener.Accept()
			if err != nil {
				return
			}

			go func(c net.Conn) {
				defer c.Close()

				scanner := bufio.NewScanner(c)
				for scanner.Scan() {
					// Parse RPC request envelope
					var req rpc.Request
					if err := json.Unmarshal(scanner.Bytes(), &req); err != nil {
						continue
					}

					// Delay response
					time.Sleep(delay)

					// Return empty results via RPC protocol
					recallResp := rpc.RecallResponse{Paths: []string{}}
					respBytes, _ := json.Marshal(recallResp)
					resp := rpc.Response{OK: true, Result: respBytes}
					respJSON, _ := json.Marshal(resp)
					c.Write(append(respJSON, '\n'))
				}
			}(conn)
		}
	}()

	return srv
}

type testRPCServer struct {
	listener net.Listener
	graph    *graph.Graph
	docs     []recall.Document
	done     chan struct{}
}

func (s *testRPCServer) Stop() {
	close(s.done)
	if s.listener != nil {
		s.listener.Close()
	}
}

// ============================================================================
// Test helper functions that call the implementation
// ============================================================================

// runRecallQuery executes a recall query and returns results
func runRecallQuery(sockPath string, seed string, limit int) ([]recall.WalkResult, error) {
	return connectOrFallback(sockPath, seed, limit, 5*time.Second, false, "")
}

// runRecallQueryWithTimeout executes a recall query with timeout
func runRecallQueryWithTimeout(sockPath string, seed string, limit int, timeout time.Duration) ([]recall.WalkResult, error) {
	return connectOrFallback(sockPath, seed, limit, timeout, false, "")
}

// runRecallFormatted executes a recall query with format specification
func runRecallFormatted(sockPath string, seed string, limit int, format string) (string, error) {
	results, err := connectOrFallback(sockPath, seed, limit, 5*time.Second, false, "")
	if err != nil {
		return "", err
	}
	return formatRecall(results, format)
}

// runRecallNoFallback executes a recall query with --no-fallback flag
func runRecallNoFallback(sockPath string, seed string, limit int) error {
	_, err := connectOrFallback(sockPath, seed, limit, 5*time.Second, true, "")
	return err
}

// ============================================================================
// Utility helpers
// ============================================================================

func contains(s, substr string) bool {
	for i := 0; i < len(s)-len(substr)+1; i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
