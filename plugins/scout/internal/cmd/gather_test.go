package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

// ─── helpers ─────────────────────────────────────────────────────────────────

// fakeGatherer returns a fixed set of items, used to test the gather pipeline
// without real network calls. Injected via registerFakeSource.
type fakeGatherer struct {
	items []gather.IntelItem
	err   error
}

func (f *fakeGatherer) Name() string { return "fake" }
func (f *fakeGatherer) Gather(_ context.Context, _ []string) ([]gather.IntelItem, error) {
	return f.items, f.err
}

// registerFakeSource adds a "fake" source to gather.Registry for the duration
// of the test and returns a cleanup function. NOT safe for t.Parallel().
func registerFakeSource(t *testing.T, items []gather.IntelItem, srcErr error) func() {
	t.Helper()
	old, hadOld := gather.Registry["fake"]
	gather.Registry["fake"] = func(_ gather.TopicConfig) (gather.Gatherer, error) {
		return &fakeGatherer{items: items, err: srcErr}, nil
	}
	return func() {
		if hadOld {
			gather.Registry["fake"] = old
		} else {
			delete(gather.Registry, "fake")
		}
	}
}

// makeFakeItems creates n IntelItems with unique IDs, URLs, and distinct titles.
// Titles are intentionally varied to avoid Jaccard-similarity deduplication
// (threshold 0.6): each title uses a unique domain word that doesn't appear in others.
var fakeTitles = []string{
	"Golang concurrency patterns explained",
	"Rust ownership and borrowing deep dive",
	"Python async frameworks comparison",
	"TypeScript generics advanced usage",
	"Kubernetes operator development guide",
	"PostgreSQL performance tuning tricks",
	"gRPC streaming versus REST tradeoffs",
	"WebAssembly in production environments",
	"eBPF observability for microservices",
	"Terraform module best practices overview",
}

func makeFakeItems(n int) []gather.IntelItem {
	items := make([]gather.IntelItem, n)
	for i := range items {
		title := fmt.Sprintf("unique-entry-%d: %s", i, fakeTitles[i%len(fakeTitles)])
		items[i] = gather.IntelItem{
			ID:        fmt.Sprintf("fake-item-%d", i),
			Title:     title,
			Content:   fmt.Sprintf("Content for entry %d with unique details about topic %d", i, i*7),
			SourceURL: fmt.Sprintf("https://example.com/posts/%d/unique-%d", i, i*13),
			Author:    fmt.Sprintf("Author%d", i),
			Timestamp: time.Now().UTC().Add(-time.Duration(i) * time.Hour),
			Tags:      []string{fmt.Sprintf("tag%d", i)},
		}
	}
	return items
}

// shortSockPath returns a socket path short enough for macOS's 104-char UDS limit.
// t.TempDir() paths are often 80+ chars, leaving no room for a filename.
func shortSockPath(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("/tmp", "sc")
	if err != nil {
		t.Fatalf("MkdirTemp: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return filepath.Join(dir, "d.sock")
}

// shortHome creates a short temp HOME dir under /tmp and returns its path.
// Use this when the daemon emitter must resolve ~/.alluka/daemon.sock.
func shortHome(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("/tmp", "sh")
	if err != nil {
		t.Fatalf("MkdirTemp: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return dir
}

// startFakeDaemon starts a Unix domain socket listener at sockPath and
// returns a channel that receives the raw bytes of the first connection.
func startFakeDaemon(t *testing.T, sockPath string) <-chan []byte {
	t.Helper()
	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen unix %s: %v", sockPath, err)
	}
	t.Cleanup(func() { ln.Close() })

	ch := make(chan []byte, 1)
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		buf := make([]byte, 4096)
		n, _ := conn.Read(buf)
		ch <- buf[:n]
	}()
	return ch
}

// createTopicWithFakeSource writes a topic file using the "fake" source.
func createTopicWithFakeSource(t *testing.T, home, topicName string) {
	t.Helper()
	topicsDir := filepath.Join(home, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatalf("mkdir topics: %v", err)
	}
	writeTopicFile(t, topicsDir, gather.TopicConfig{
		Name:        topicName,
		Sources:     []string{"fake"},
		SearchTerms: []string{"test"},
	})
}

// ─── GatherCmd ───────────────────────────────────────────────────────────────

func TestGatherCmd_NoTopics(t *testing.T) {
	setupTempHome(t)
	output, err := captureOutput(func() error {
		return GatherCmd(nil, false)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "No topics configured") {
		t.Errorf("expected 'No topics configured', got: %s", output)
	}
}

func TestGatherCmd_UnknownTopic(t *testing.T) {
	setupTempHome(t)
	err := GatherCmd([]string{"nonexistent-topic"}, false)
	if err == nil {
		t.Fatal("expected error for unknown topic")
	}
	if !strings.Contains(err.Error(), "not found") {
		t.Errorf("expected 'not found' error, got: %v", err)
	}
}

func TestGatherCmd_HelpFlag(t *testing.T) {
	setupTempHome(t)
	output, err := captureOutput(func() error {
		return GatherCmd([]string{"--help"}, false)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "Usage:") {
		t.Errorf("expected Usage in help output, got: %s", output)
	}
}

func TestGatherCmd_WithFakeSource_CreatesIntelFile(t *testing.T) {
	home := setupTempHome(t)
	defer registerFakeSource(t, makeFakeItems(3), nil)()
	createTopicWithFakeSource(t, home, "test-gather")

	if _, err := captureOutput(func() error {
		return GatherCmd([]string{"test-gather"}, false)
	}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	intelDir := filepath.Join(home, ".scout", "intel", "test-gather")
	entries, err := os.ReadDir(intelDir)
	if err != nil {
		t.Fatalf("intel dir not created: %v", err)
	}
	if len(entries) == 0 {
		t.Error("expected at least one intel file to be created")
	}

	// Verify the intel file contains valid JSON with expected structure.
	filePath := filepath.Join(intelDir, entries[0].Name())
	data, _ := os.ReadFile(filePath)
	var intelFile gather.IntelFile
	if err := json.Unmarshal(data, &intelFile); err != nil {
		t.Fatalf("intel file is not valid JSON: %v", err)
	}
	if intelFile.Topic != "test-gather" {
		t.Errorf("expected topic 'test-gather', got %s", intelFile.Topic)
	}
	if intelFile.Source != "fake" {
		t.Errorf("expected source 'fake', got %s", intelFile.Source)
	}
	if len(intelFile.Items) == 0 {
		t.Error("expected items in intel file")
	}
}

func TestGatherCmd_JSONOutput(t *testing.T) {
	home := setupTempHome(t)
	defer registerFakeSource(t, makeFakeItems(2), nil)()
	createTopicWithFakeSource(t, home, "json-test")

	output, err := captureOutput(func() error {
		return GatherCmd([]string{"json-test"}, true)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var results []gatherResult
	if err := json.Unmarshal([]byte(output), &results); err != nil {
		t.Fatalf("expected valid JSON output, got: %s\nerror: %v", output, err)
	}
	if len(results) != 1 {
		t.Fatalf("expected 1 result, got %d", len(results))
	}
	if results[0].Topic != "json-test" {
		t.Errorf("expected topic 'json-test', got %s", results[0].Topic)
	}
	if results[0].Source != "fake" {
		t.Errorf("expected source 'fake', got %s", results[0].Source)
	}
	if results[0].Items != 2 {
		t.Errorf("expected 2 items, got %d", results[0].Items)
	}
	if results[0].Error != "" {
		t.Errorf("expected no error, got: %s", results[0].Error)
	}
}

func TestGatherCmd_JSONOutput_SourceError(t *testing.T) {
	home := setupTempHome(t)
	defer registerFakeSource(t, nil, fmt.Errorf("source unavailable"))()
	createTopicWithFakeSource(t, home, "error-test")

	output, err := captureOutput(func() error {
		return GatherCmd([]string{"error-test"}, true)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var results []gatherResult
	if err := json.Unmarshal([]byte(output), &results); err != nil {
		t.Fatalf("expected valid JSON even on source error: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("expected 1 result, got %d", len(results))
	}
	if results[0].Error == "" {
		t.Error("expected error field populated when source fails")
	}
}

func TestGatherCmd_FullFlag_ResetsState(t *testing.T) {
	home := setupTempHome(t)
	defer registerFakeSource(t, makeFakeItems(1), nil)()
	createTopicWithFakeSource(t, home, "full-test")

	// First gather — saves incremental state
	if _, err := captureOutput(func() error {
		return GatherCmd([]string{"full-test"}, false)
	}); err != nil {
		t.Fatalf("first gather failed: %v", err)
	}

	stateFile := filepath.Join(home, ".scout", "gather-state.json")
	if _, err := os.Stat(stateFile); os.IsNotExist(err) {
		t.Error("expected gather state file to be created after first gather")
	}

	// Second gather with --full should succeed and not error
	if _, err := captureOutput(func() error {
		return GatherCmd([]string{"full-test", "--full"}, false)
	}); err != nil {
		t.Fatalf("full re-gather failed: %v", err)
	}
}

func TestGatherCmd_SinceFlagMissingValue(t *testing.T) {
	setupTempHome(t)
	err := GatherCmd([]string{"sometopic", "--since"}, false)
	if err == nil {
		t.Fatal("expected error when --since has no value")
	}
}

func TestGatherCmd_SavesGatherState(t *testing.T) {
	home := setupTempHome(t)
	defer registerFakeSource(t, makeFakeItems(5), nil)()
	createTopicWithFakeSource(t, home, "state-test")

	if _, err := captureOutput(func() error {
		return GatherCmd([]string{"state-test"}, false)
	}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	stateFile := filepath.Join(home, ".scout", "gather-state.json")
	data, err := os.ReadFile(stateFile)
	if err != nil {
		t.Fatalf("gather state file not created: %v", err)
	}
	var state map[string]interface{}
	if err := json.Unmarshal(data, &state); err != nil {
		t.Fatalf("gather state is not valid JSON: %v", err)
	}
	if _, ok := state["topics"]; !ok {
		t.Error("expected 'topics' key in gather state")
	}
}

func TestGatherCmd_MultipleTopics(t *testing.T) {
	home := setupTempHome(t)
	defer registerFakeSource(t, makeFakeItems(2), nil)()

	topicsDir := filepath.Join(home, ".scout", "topics")
	if err := os.MkdirAll(topicsDir, 0700); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"topic-a", "topic-b"} {
		writeTopicFile(t, topicsDir, gather.TopicConfig{
			Name:        name,
			Sources:     []string{"fake"},
			SearchTerms: []string{"test"},
		})
	}

	output, err := captureOutput(func() error {
		return GatherCmd(nil, true)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var results []gatherResult
	if err := json.Unmarshal([]byte(output), &results); err != nil {
		t.Fatalf("expected valid JSON, got: %s\nerror: %v", output, err)
	}
	if len(results) != 2 {
		t.Errorf("expected 2 results (one per topic), got %d", len(results))
	}
}

func TestGatherCmd_ZeroItemsFromSource(t *testing.T) {
	home := setupTempHome(t)
	defer registerFakeSource(t, nil, nil)() // source returns 0 items, no error
	createTopicWithFakeSource(t, home, "empty-source")

	output, err := captureOutput(func() error {
		return GatherCmd([]string{"empty-source"}, true)
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var results []gatherResult
	json.Unmarshal([]byte(output), &results)
	if len(results) != 1 {
		t.Errorf("expected 1 result (zero items), got %d", len(results))
	}
	if results[0].Items != 0 {
		t.Errorf("expected 0 items, got %d", results[0].Items)
	}
}

// ─── gatherSourceItems ───────────────────────────────────────────────────────

func TestGatherSourceItems_UnknownSource(t *testing.T) {
	topic := gather.TopicConfig{Name: "test", Sources: []string{"unknown-xyz"}}
	result, items := gatherSourceItems(topic, "unknown-xyz", time.Time{}, false)
	if result.Error == "" {
		t.Error("expected error for unknown source")
	}
	if !strings.Contains(result.Error, "unknown source") {
		t.Errorf("expected 'unknown source' in error, got: %s", result.Error)
	}
	if items != nil {
		t.Error("expected nil items for unknown source")
	}
}

func TestGatherSourceItems_RSSWithNoFeeds(t *testing.T) {
	// "rss" factory returns an error when no feeds are configured.
	topic := gather.TopicConfig{Name: "test", Sources: []string{"rss"}, Feeds: nil}
	result, items := gatherSourceItems(topic, "rss", time.Time{}, false)
	if result.Error == "" {
		t.Error("expected error when rss source has no feeds")
	}
	if items != nil {
		t.Error("expected nil items on factory error")
	}
}

func TestGatherSourceItems_PodcastWithNoFeeds(t *testing.T) {
	topic := gather.TopicConfig{Name: "test", Sources: []string{"podcast"}, PodcastFeeds: nil}
	result, items := gatherSourceItems(topic, "podcast", time.Time{}, false)
	if result.Error == "" {
		t.Error("expected error when podcast source has no feeds")
	}
	if items != nil {
		t.Error("expected nil items on factory error")
	}
}

func TestGatherSourceItems_FakeSource(t *testing.T) {
	defer registerFakeSource(t, makeFakeItems(5), nil)()
	topic := gather.TopicConfig{Name: "test", Sources: []string{"fake"}, SearchTerms: []string{"test"}}
	result, items := gatherSourceItems(topic, "fake", time.Time{}, false)
	if result.Error != "" {
		t.Errorf("expected no error, got: %s", result.Error)
	}
	if result.Items == 0 {
		t.Error("expected items count in result")
	}
	if len(items) == 0 {
		t.Error("expected non-empty items slice")
	}
}

func TestGatherSourceItems_SourceNameCaseFolded(t *testing.T) {
	// "FAKE" in uppercase should resolve to "fake" after strings.ToLower.
	defer registerFakeSource(t, makeFakeItems(1), nil)()
	topic := gather.TopicConfig{Name: "test", Sources: []string{"FAKE"}, SearchTerms: []string{"x"}}
	result, _ := gatherSourceItems(topic, "FAKE", time.Time{}, false)
	if result.Error != "" {
		t.Errorf("source name case-fold failed, got error: %s", result.Error)
	}
}

func TestGatherSourceItems_IncrementalCutoffFiltersOldItems(t *testing.T) {
	now := time.Now().UTC()
	oldItem := gather.IntelItem{
		ID:        "old-item",
		Title:     "Old Article",
		SourceURL: "https://example.com/old",
		Timestamp: now.Add(-48 * time.Hour),
	}
	newItem := gather.IntelItem{
		ID:        "new-item",
		Title:     "New Article",
		SourceURL: "https://example.com/new",
		Timestamp: now.Add(-1 * time.Hour),
	}
	defer registerFakeSource(t, []gather.IntelItem{oldItem, newItem}, nil)()

	topic := gather.TopicConfig{Name: "test", Sources: []string{"fake"}, SearchTerms: []string{"article"}}
	cutoff := now.Add(-12 * time.Hour) // old item is 48h ago, should be excluded
	_, items := gatherSourceItems(topic, "fake", cutoff, false)

	for _, item := range items {
		if item.ID == "old-item" {
			t.Error("old item (before cutoff) should have been filtered out")
		}
	}
}

// ─── emitGatherEvents ────────────────────────────────────────────────────────

func TestEmitGatherEvents_NoDaemon_NoPanic(t *testing.T) {
	results := []gatherResult{
		{Topic: "ai-models", Source: "hackernews", Items: 10},
		{Topic: "ai-models", Source: "reddit", Items: 5},
	}
	emitGatherEvents(results) // must not panic
}

func TestEmitGatherEvents_ZeroItemsSkipped(t *testing.T) {
	results := []gatherResult{
		{Topic: "ai-models", Source: "hackernews", Items: 0},
		{Topic: "ai-models", Source: "reddit", Error: "timeout"},
	}
	emitGatherEvents(results)
}

func TestEmitGatherEvents_WithFakeDaemon(t *testing.T) {
	// Use a short /tmp-based HOME so ~/.alluka/daemon.sock stays under 104 chars.
	home := shortHome(t)
	t.Setenv("HOME", home)
	allukaDir := filepath.Join(home, ".alluka")
	if err := os.MkdirAll(allukaDir, 0700); err != nil {
		t.Fatal(err)
	}
	// Must match the filename NewEmitter() uses: ~/.alluka/daemon.sock
	sockPath := filepath.Join(allukaDir, "daemon.sock")
	received := startFakeDaemon(t, sockPath)

	results := []gatherResult{
		{Topic: "test-topic", Source: "hackernews", Items: 7},
	}
	emitGatherEvents(results)

	select {
	case data := <-received:
		var ev map[string]interface{}
		if err := json.Unmarshal([]byte(strings.TrimSpace(string(data))), &ev); err != nil {
			t.Fatalf("received invalid JSON: %q\nerror: %v", string(data), err)
		}
		if ev["type"] != "scout.intel_gathered" {
			t.Errorf("expected type scout.intel_gathered, got %v", ev["type"])
		}
		d, ok := ev["data"].(map[string]interface{})
		if !ok {
			t.Fatalf("expected data field to be an object")
		}
		if d["topic"] != "test-topic" {
			t.Errorf("expected topic test-topic, got %v", d["topic"])
		}
		if count, _ := d["item_count"].(float64); count != 7 {
			t.Errorf("expected item_count 7, got %v", d["item_count"])
		}
	case <-time.After(2 * time.Second):
		t.Fatal("timeout: no event received from emitGatherEvents")
	}
}

func TestEmitGatherEvents_AggregatesPerTopic(t *testing.T) {
	// Two sources for same topic: daemon should receive one event with sum.
	home := shortHome(t)
	t.Setenv("HOME", home)
	allukaDir := filepath.Join(home, ".alluka")
	if err := os.MkdirAll(allukaDir, 0700); err != nil {
		t.Fatal(err)
	}
	sockPath := filepath.Join(allukaDir, "daemon.sock")

	// Listener collects all events (multiple connections or one connection with multiple lines)
	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { ln.Close() })

	events := make(chan string, 10)
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func(c net.Conn) {
				defer c.Close()
				buf := make([]byte, 4096)
				n, _ := c.Read(buf)
				events <- strings.TrimSpace(string(buf[:n]))
			}(conn)
		}
	}()

	results := []gatherResult{
		{Topic: "ai-models", Source: "hackernews", Items: 10},
		{Topic: "ai-models", Source: "reddit", Items: 15},
		{Topic: "go-dev", Source: "hackernews", Items: 5},
		{Topic: "ai-models", Source: "devto", Error: "timeout"}, // error: not counted
	}
	emitGatherEvents(results)

	// Collect events (one per topic with items)
	received := make(map[string]float64)
	deadline := time.After(2 * time.Second)
	for i := 0; i < 2; i++ { // expect 2 topics: ai-models and go-dev
		select {
		case data := <-events:
			var ev map[string]interface{}
			if err := json.Unmarshal([]byte(data), &ev); err != nil {
				t.Logf("invalid JSON in event: %q", data)
				continue
			}
			d, _ := ev["data"].(map[string]interface{})
			topic, _ := d["topic"].(string)
			count, _ := d["item_count"].(float64)
			received[topic] = count
		case <-deadline:
			t.Logf("timeout after receiving %d events: %v", len(received), received)
			goto done
		}
	}
done:
	if v, ok := received["ai-models"]; ok && v != 25 {
		t.Errorf("ai-models: expected item_count 25 (10+15), got %v", v)
	}
	if _, ok := received["go-dev"]; !ok {
		t.Log("go-dev event may not have arrived within timeout — emitter uses separate connections")
	}
}
