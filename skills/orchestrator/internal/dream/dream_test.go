package dream

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// ─── JSONL fixture helpers ────────────────────────────────────────────────────

func sysInitLine(cwd string) []byte {
	b, _ := json.Marshal(map[string]any{"type": "system", "subtype": "init", "cwd": cwd})
	return b
}

func userLine(content string) []byte {
	b, _ := json.Marshal(map[string]any{
		"type":    "user",
		"message": map[string]any{"role": "user", "content": content},
	})
	return b
}

func userLineBlocks(texts ...string) []byte {
	var blocks []map[string]any
	for _, t := range texts {
		blocks = append(blocks, map[string]any{"type": "text", "text": t})
	}
	b, _ := json.Marshal(map[string]any{
		"type":    "user",
		"message": map[string]any{"role": "user", "content": blocks},
	})
	return b
}

func assistantLine(text string) []byte {
	b, _ := json.Marshal(map[string]any{
		"type": "assistant",
		"message": map[string]any{
			"role":    "assistant",
			"content": []map[string]any{{"type": "text", "text": text}},
		},
	})
	return b
}

// writeJSONL creates a temp *.jsonl file from the given lines and returns its path.
func writeJSONL(t *testing.T, dir string, lines [][]byte) string {
	t.Helper()
	f, err := os.CreateTemp(dir, "*.jsonl")
	if err != nil {
		t.Fatalf("create temp file: %v", err)
	}
	for _, line := range lines {
		f.Write(line)
		f.WriteString("\n")
	}
	f.Close()
	return f.Name()
}

// ─── fakeStore ────────────────────────────────────────────────────────────────

type fakeStore struct {
	alwaysProcessed bool
	processedFiles  map[string]bool
	processedChunks map[string]bool
	markFileCalls   int
	markChunkCalls  int
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		processedFiles:  make(map[string]bool),
		processedChunks: make(map[string]bool),
	}
}

func (f *fakeStore) IsFileProcessed(hash string) (bool, error) {
	return f.alwaysProcessed || f.processedFiles[hash], nil
}

func (f *fakeStore) MarkFileProcessed(path, hash string, msgCount, chunkCount int) error {
	f.markFileCalls++
	f.processedFiles[hash] = true
	return nil
}

func (f *fakeStore) IsChunkProcessed(hash string) (bool, error) {
	return f.processedChunks[hash], nil
}

func (f *fakeStore) MarkChunkProcessed(transcriptPath, hash string, idx int) error {
	f.markChunkCalls++
	f.processedChunks[hash] = true
	return nil
}

func (f *fakeStore) Status() (int, int, error) {
	return len(f.processedFiles), len(f.processedChunks), nil
}

func (f *fakeStore) Reset() error {
	f.processedFiles = make(map[string]bool)
	f.processedChunks = make(map[string]bool)
	f.markFileCalls = 0
	f.markChunkCalls = 0
	return nil
}

func (f *fakeStore) Close() error { return nil }

// ─── ParseTranscript ─────────────────────────────────────────────────────────

func TestParseTranscript(t *testing.T) {
	dir := t.TempDir()

	tests := []struct {
		name     string
		lines    [][]byte
		wantMsgs int
		wantCwd  string
	}{
		{
			name:     "empty file produces no messages",
			wantMsgs: 0,
		},
		{
			name:     "malformed lines are skipped silently",
			lines:    [][]byte{[]byte(`{bad json`), []byte(`also bad`), userLine("valid message")},
			wantMsgs: 1,
		},
		{
			name:     "system init line extracts cwd",
			lines:    [][]byte{sysInitLine("/home/alice/project"), userLine("hi"), assistantLine("hello")},
			wantMsgs: 2,
			wantCwd:  "/home/alice/project",
		},
		{
			name:     "user message with plain string content parsed",
			lines:    [][]byte{userLine("plain string content here")},
			wantMsgs: 1,
		},
		{
			name:     "user message with block array content parsed",
			lines:    [][]byte{userLineBlocks("block text one", "block text two")},
			wantMsgs: 1,
		},
		{
			name:     "empty string content message skipped",
			lines:    [][]byte{userLine("")},
			wantMsgs: 0,
		},
		{
			name:     "empty block array content message skipped",
			lines:    [][]byte{userLineBlocks()},
			wantMsgs: 0,
		},
		{
			name:     "assistant message extracted",
			lines:    [][]byte{assistantLine("I can help with that.")},
			wantMsgs: 1,
		},
		{
			name: "mix of valid and invalid lines",
			lines: [][]byte{
				sysInitLine("/tmp/proj"),
				[]byte("not json at all"),
				userLine("hello"),
				[]byte(`{"type":"unknown","data":{}}`),
				assistantLine("world"),
			},
			wantMsgs: 2,
			wantCwd:  "/tmp/proj",
		},
		{
			name:     "blank lines in file are skipped",
			lines:    [][]byte{[]byte(""), userLine("msg"), []byte("")},
			wantMsgs: 1,
		},
		{
			name:     "multiple malformed lines before valid line",
			lines:    [][]byte{[]byte("!!!"), []byte("{"), []byte("}garbage"), assistantLine("ok")},
			wantMsgs: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			path := writeJSONL(t, dir, tc.lines)
			msgs, cwd, err := ParseTranscript(path)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if len(msgs) != tc.wantMsgs {
				t.Errorf("messages: got %d, want %d", len(msgs), tc.wantMsgs)
			}
			if cwd != tc.wantCwd {
				t.Errorf("cwd: got %q, want %q", cwd, tc.wantCwd)
			}
		})
	}
}

func TestParseTranscript_nonexistent_file_returns_error(t *testing.T) {
	_, _, err := ParseTranscript("/nonexistent/path/does_not_exist.jsonl")
	if err == nil {
		t.Fatal("expected error for nonexistent file, got nil")
	}
}

func TestParseTranscript_tool_use_blocks_excluded(t *testing.T) {
	dir := t.TempDir()
	// Content array with only tool_use block → no text → message skipped.
	line, _ := json.Marshal(map[string]any{
		"type": "assistant",
		"message": map[string]any{
			"role": "assistant",
			"content": []map[string]any{
				{"type": "tool_use", "id": "x", "name": "Bash", "input": map[string]any{"command": "ls"}},
			},
		},
	})
	path := writeJSONL(t, dir, [][]byte{line})
	msgs, _, err := ParseTranscript(path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(msgs) != 0 {
		t.Errorf("expected 0 messages (tool_use only), got %d", len(msgs))
	}
}

func TestParseTranscript_seq_nums_are_1indexed(t *testing.T) {
	dir := t.TempDir()
	path := writeJSONL(t, dir, [][]byte{
		sysInitLine("/tmp"),         // line 1 — system, no ConvMessage
		userLine("first message"),   // line 2
		assistantLine("first reply"), // line 3
	})
	msgs, _, err := ParseTranscript(path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(msgs) != 2 {
		t.Fatalf("expected 2 messages, got %d", len(msgs))
	}
	if msgs[0].SeqNum != 2 {
		t.Errorf("msgs[0].SeqNum = %d, want 2", msgs[0].SeqNum)
	}
	if msgs[1].SeqNum != 3 {
		t.Errorf("msgs[1].SeqNum = %d, want 3", msgs[1].SeqNum)
	}
}

// ─── isWorkerSession ──────────────────────────────────────────────────────────

func TestIsWorkerSession(t *testing.T) {
	tests := []struct {
		cwd  string
		want bool
	}{
		{"", false},
		{"/home/user/project", false},
		{"/Users/alice/code/nanika", false},
		{"/home/alice/.alluka/worktrees/abc123/nanika", true},
		{"/home/alice/.alluka/workers/alpha", true},
		{"/home/alice/.via/worktrees/mission-x/repo", true},
		{"/alluka/worktrees/something", false}, // no /. prefix
		{"/home/user/.alluka/worktrees/", true},
		{"/home/user/.alluka/workers/", true},
	}

	for _, tc := range tests {
		tc := tc
		t.Run("cwd="+tc.cwd, func(t *testing.T) {
			got := isWorkerSession(tc.cwd)
			if got != tc.want {
				t.Errorf("isWorkerSession(%q) = %v, want %v", tc.cwd, got, tc.want)
			}
		})
	}
}

// ─── ChunkSession ─────────────────────────────────────────────────────────────

func TestChunkSession(t *testing.T) {
	shortMsg := ConvMessage{Role: "user", Text: strings.Repeat("a", 40)}    // ~10 tokens
	bigMsg := ConvMessage{Role: "user", Text: strings.Repeat("b", 40000)}   // ~10000 tokens

	tests := []struct {
		name       string
		msgs       []ConvMessage
		maxTokens  int
		wantChunks int
	}{
		{
			name:       "nil messages produces no chunks",
			msgs:       nil,
			maxTokens:  6000,
			wantChunks: 0,
		},
		{
			name:       "empty messages slice produces no chunks",
			msgs:       []ConvMessage{},
			maxTokens:  6000,
			wantChunks: 0,
		},
		{
			name:       "single short message fits in one chunk",
			msgs:       []ConvMessage{shortMsg},
			maxTokens:  6000,
			wantChunks: 1,
		},
		{
			name:       "single oversized message placed alone not dropped",
			msgs:       []ConvMessage{bigMsg},
			maxTokens:  100,
			wantChunks: 1,
		},
		{
			name:       "two short messages within budget stay together",
			msgs:       []ConvMessage{shortMsg, shortMsg},
			maxTokens:  6000,
			wantChunks: 1,
		},
		{
			name:       "two oversized messages split into two chunks",
			msgs:       []ConvMessage{bigMsg, bigMsg},
			maxTokens:  100,
			wantChunks: 2,
		},
		{
			name:       "zero maxTokens uses default 6000",
			msgs:       []ConvMessage{shortMsg, shortMsg},
			maxTokens:  0,
			wantChunks: 1,
		},
		{
			name:       "short then oversized then short splits into three chunks",
			msgs:       []ConvMessage{shortMsg, bigMsg, shortMsg},
			maxTokens:  100,
			wantChunks: 3,
		},
		{
			name: "many short messages all under budget stay in one chunk",
			msgs: func() []ConvMessage {
				var ms []ConvMessage
				for i := 0; i < 10; i++ {
					ms = append(ms, ConvMessage{Role: "user", Text: "hello"})
				}
				return ms
			}(),
			maxTokens:  6000,
			wantChunks: 1,
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			sess := Session{Ref: TranscriptRef{Path: "/test.jsonl"}, Messages: tc.msgs}
			chunks := ChunkSession(sess, tc.maxTokens)
			if len(chunks) != tc.wantChunks {
				t.Errorf("chunks: got %d, want %d", len(chunks), tc.wantChunks)
			}
		})
	}
}

func TestChunkSession_chunk_properties(t *testing.T) {
	msg := ConvMessage{Role: "user", Text: "explain this code"}
	sess := Session{
		Ref:      TranscriptRef{Path: "/my/path.jsonl"},
		Messages: []ConvMessage{msg, msg},
	}
	chunks := ChunkSession(sess, 6000)
	if len(chunks) != 1 {
		t.Fatalf("expected 1 chunk, got %d", len(chunks))
	}
	c := chunks[0]
	if c.SessionPath != "/my/path.jsonl" {
		t.Errorf("SessionPath = %q, want /my/path.jsonl", c.SessionPath)
	}
	if c.ChunkIndex != 0 {
		t.Errorf("ChunkIndex = %d, want 0", c.ChunkIndex)
	}
	if c.MsgCount != 2 {
		t.Errorf("MsgCount = %d, want 2", c.MsgCount)
	}
	if c.Hash == "" {
		t.Error("Hash must not be empty")
	}
	if c.Text == "" {
		t.Error("Text must not be empty")
	}
}

func TestChunkSession_indices_are_sequential(t *testing.T) {
	bigText := strings.Repeat("x", 400) // ~100 tokens → exceeds budget of 50
	msgs := []ConvMessage{
		{Role: "user", Text: bigText},
		{Role: "assistant", Text: bigText},
		{Role: "user", Text: bigText},
	}
	sess := Session{Ref: TranscriptRef{Path: "/test.jsonl"}, Messages: msgs}
	chunks := ChunkSession(sess, 50)

	if len(chunks) != 3 {
		t.Fatalf("expected 3 chunks, got %d", len(chunks))
	}
	for i, c := range chunks {
		if c.ChunkIndex != i {
			t.Errorf("chunks[%d].ChunkIndex = %d, want %d", i, c.ChunkIndex, i)
		}
	}
}

func TestChunkSession_all_messages_covered(t *testing.T) {
	// Every message must appear in exactly one chunk — none dropped, none duplicated.
	msgs := []ConvMessage{
		{Role: "user", Text: strings.Repeat("u", 100)},
		{Role: "assistant", Text: strings.Repeat("a", 100)},
		{Role: "user", Text: strings.Repeat("b", 100)},
		{Role: "assistant", Text: strings.Repeat("c", 100)},
		{Role: "user", Text: strings.Repeat("d", 100)},
	}
	sess := Session{Ref: TranscriptRef{Path: "/test.jsonl"}, Messages: msgs}
	// Budget of 50 tokens → each message is ~25 tokens → two messages per chunk.
	chunks := ChunkSession(sess, 50)

	total := 0
	for _, c := range chunks {
		total += c.MsgCount
	}
	if total != len(msgs) {
		t.Errorf("total messages across chunks = %d, want %d (messages lost or duplicated)", total, len(msgs))
	}
}

// ─── estimateTokens ───────────────────────────────────────────────────────────

func TestEstimateTokens(t *testing.T) {
	tests := []struct {
		text string
		want int
	}{
		{"", 0},
		{"abcd", 1},        // (4+3)/4 = 1
		{"hello", 2},       // (5+3)/4 = 2
		{"12345678", 2},    // (8+3)/4 = 2
		{"123456789012", 3}, // (12+3)/4 = 3
		{strings.Repeat("x", 400), 100}, // (400+3)/4 = 100
	}

	for _, tc := range tests {
		tc := tc
		t.Run("len="+strings.Repeat("_", len(tc.text)/100), func(t *testing.T) {
			got := estimateTokens(tc.text)
			if got != tc.want {
				t.Errorf("estimateTokens(len=%d) = %d, want %d", len(tc.text), got, tc.want)
			}
		})
	}
}

// ─── chunkHash ────────────────────────────────────────────────────────────────

func TestChunkHash(t *testing.T) {
	t.Run("deterministic", func(t *testing.T) {
		text := "User: hello\n\nAssistant: world"
		if chunkHash(text) != chunkHash(text) {
			t.Error("non-deterministic hash")
		}
	})

	t.Run("case insensitive", func(t *testing.T) {
		if chunkHash("HELLO WORLD") != chunkHash("hello world") {
			t.Error("hash is case-sensitive, want case-insensitive")
		}
	})

	t.Run("leading and trailing whitespace trimmed", func(t *testing.T) {
		if chunkHash("  hello  ") != chunkHash("hello") {
			t.Error("hash is whitespace-sensitive, want trimmed")
		}
	})

	t.Run("different text produces different hash", func(t *testing.T) {
		if chunkHash("hello") == chunkHash("world") {
			t.Error("collision: different texts produced same hash")
		}
	})

	t.Run("hash is 64-char hex", func(t *testing.T) {
		h := chunkHash("test content")
		if len(h) != 64 {
			t.Errorf("expected 64-char hex hash, got len=%d: %s", len(h), h)
		}
	})
}

// ─── SQLiteStore ─────────────────────────────────────────────────────────────

func openTestStore(t *testing.T) *SQLiteStore {
	t.Helper()
	s, err := OpenSQLiteStore(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatalf("OpenSQLiteStore: %v", err)
	}
	t.Cleanup(func() { s.Close() })
	return s
}

func TestSQLiteStore_file_not_processed_initially(t *testing.T) {
	s := openTestStore(t)
	got, err := s.IsFileProcessed("deadbeef123")
	if err != nil {
		t.Fatalf("IsFileProcessed: %v", err)
	}
	if got {
		t.Error("expected false for unknown hash")
	}
}

func TestSQLiteStore_mark_file_then_check(t *testing.T) {
	s := openTestStore(t)
	const hash = "abc123file"
	if err := s.MarkFileProcessed("/tmp/t.jsonl", hash, 10, 2); err != nil {
		t.Fatalf("MarkFileProcessed: %v", err)
	}
	got, err := s.IsFileProcessed(hash)
	if err != nil {
		t.Fatalf("IsFileProcessed: %v", err)
	}
	if !got {
		t.Error("expected true after MarkFileProcessed")
	}
}

func TestSQLiteStore_mark_file_idempotent(t *testing.T) {
	s := openTestStore(t)
	const hash = "dupfile123"
	if err := s.MarkFileProcessed("/tmp/t.jsonl", hash, 5, 1); err != nil {
		t.Fatalf("first MarkFileProcessed: %v", err)
	}
	// INSERT OR REPLACE: second call must succeed.
	if err := s.MarkFileProcessed("/tmp/t.jsonl", hash, 5, 1); err != nil {
		t.Fatalf("second MarkFileProcessed (idempotent): %v", err)
	}
}

func TestSQLiteStore_chunk_not_processed_initially(t *testing.T) {
	s := openTestStore(t)
	got, err := s.IsChunkProcessed("chunk_hash_xyz")
	if err != nil {
		t.Fatalf("IsChunkProcessed: %v", err)
	}
	if got {
		t.Error("expected false for unknown chunk hash")
	}
}

func TestSQLiteStore_mark_chunk_then_check(t *testing.T) {
	s := openTestStore(t)
	const hash = "chunkhash1"
	if err := s.MarkChunkProcessed("/tmp/t.jsonl", hash, 0); err != nil {
		t.Fatalf("MarkChunkProcessed: %v", err)
	}
	got, err := s.IsChunkProcessed(hash)
	if err != nil {
		t.Fatalf("IsChunkProcessed: %v", err)
	}
	if !got {
		t.Error("expected true after MarkChunkProcessed")
	}
}

func TestSQLiteStore_mark_chunk_idempotent(t *testing.T) {
	s := openTestStore(t)
	const hash = "dupchunk1"
	if err := s.MarkChunkProcessed("/tmp/t.jsonl", hash, 0); err != nil {
		t.Fatalf("first MarkChunkProcessed: %v", err)
	}
	// INSERT OR IGNORE: second call must not error.
	if err := s.MarkChunkProcessed("/tmp/t.jsonl", hash, 0); err != nil {
		t.Fatalf("second MarkChunkProcessed (idempotent): %v", err)
	}
}

func TestSQLiteStore_status_counts(t *testing.T) {
	s := openTestStore(t)
	s.MarkFileProcessed("/a.jsonl", "h1", 5, 1)
	s.MarkFileProcessed("/b.jsonl", "h2", 10, 2)
	s.MarkChunkProcessed("/a.jsonl", "c1", 0)

	files, chunks, err := s.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if files != 2 {
		t.Errorf("files = %d, want 2", files)
	}
	if chunks != 1 {
		t.Errorf("chunks = %d, want 1", chunks)
	}
}

func TestSQLiteStore_reset_clears_all_state(t *testing.T) {
	s := openTestStore(t)
	s.MarkFileProcessed("/a.jsonl", "h1", 5, 1)
	s.MarkChunkProcessed("/a.jsonl", "c1", 0)

	if err := s.Reset(); err != nil {
		t.Fatalf("Reset: %v", err)
	}
	files, chunks, err := s.Status()
	if err != nil {
		t.Fatalf("Status after Reset: %v", err)
	}
	if files != 0 || chunks != 0 {
		t.Errorf("after Reset: files=%d chunks=%d, want both 0", files, chunks)
	}
}

func TestSQLiteStore_chunk_dedup_by_hash(t *testing.T) {
	// Two different transcript paths with the same chunk hash → only one row.
	s := openTestStore(t)
	const hash = "samechunkhash"
	s.MarkChunkProcessed("/a.jsonl", hash, 0)
	s.MarkChunkProcessed("/b.jsonl", hash, 0) // INSERT OR IGNORE: no duplicate

	_, chunks, err := s.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if chunks != 1 {
		t.Errorf("chunks = %d, want 1 (hash-level dedup)", chunks)
	}
}

// ─── Runner: --dry-run DB immutability ───────────────────────────────────────

func TestRunner_dryrun_no_db_writes(t *testing.T) {
	dir := t.TempDir()
	writeJSONL(t, dir, [][]byte{
		sysInitLine("/home/user/project"),
		userLine("explain this code please"),
		assistantLine("Sure, this code does X."),
		userLine("thanks a lot"),
		assistantLine("You're welcome!"),
	})

	store := newFakeStore()
	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = true
	cfg.MaxTranscriptAge = 0

	runner := NewRunner(store, nil, nil, cfg) // ldb/embedder safe when DryRun=true
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if store.markFileCalls != 0 {
		t.Errorf("DryRun: markFileCalls = %d, want 0", store.markFileCalls)
	}
	if store.markChunkCalls != 0 {
		t.Errorf("DryRun: markChunkCalls = %d, want 0", store.markChunkCalls)
	}
	// Pipeline ran up to the extraction stage.
	if report.ProcessedFiles == 0 {
		t.Error("DryRun: expected at least one ProcessedFiles")
	}
	if report.ChunksEmitted == 0 {
		t.Error("DryRun: expected chunks to be emitted (transcript was chunked)")
	}
	// No LLM calls in dry-run mode.
	if report.LLMCalls != 0 {
		t.Errorf("DryRun: LLMCalls = %d, want 0", report.LLMCalls)
	}
}

// ─── Runner: --since mtime filtering ─────────────────────────────────────────

func TestRunner_since_filter_excludes_old_transcripts(t *testing.T) {
	dir := t.TempDir()
	cutoff := time.Now()

	oldPath := writeJSONL(t, dir, [][]byte{sysInitLine("/home/user"), userLine("old"), assistantLine("old reply")})
	oldTime := cutoff.Add(-2 * time.Hour)
	os.Chtimes(oldPath, oldTime, oldTime)

	newPath := writeJSONL(t, dir, [][]byte{sysInitLine("/home/user"), userLine("new"), assistantLine("new reply")})
	newTime := cutoff.Add(time.Hour)
	os.Chtimes(newPath, newTime, newTime)

	store := newFakeStore()
	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = true
	cfg.Since = cutoff
	cfg.MaxTranscriptAge = 0

	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	// Only the new file passes the --since filter.
	if report.Discovered != 1 {
		t.Errorf("Discovered = %d, want 1 (old file excluded by --since)", report.Discovered)
	}
}

func TestRunner_since_zero_includes_all_transcripts(t *testing.T) {
	dir := t.TempDir()
	for i := 0; i < 3; i++ {
		writeJSONL(t, dir, [][]byte{
			sysInitLine("/home/user/project"),
			userLine("message"),
			assistantLine("reply"),
		})
	}

	store := newFakeStore()
	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = true
	cfg.Since = time.Time{} // zero = no since filter
	cfg.MaxTranscriptAge = 0

	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if report.Discovered != 3 {
		t.Errorf("Discovered = %d, want 3 (no since filter)", report.Discovered)
	}
}

// ─── Runner: processed marker preventing re-extraction ───────────────────────

func TestRunner_processed_marker_skips_reextraction(t *testing.T) {
	dir := t.TempDir()
	writeJSONL(t, dir, [][]byte{
		sysInitLine("/home/user/project"),
		userLine("a message here"),
		assistantLine("a reply here"),
		userLine("another message"),
		assistantLine("another reply"),
	})

	store := newFakeStore()
	store.alwaysProcessed = true // every file hash reports as already processed

	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = false // non-dry-run: would write if not for dedup
	cfg.Force = false
	cfg.MaxTranscriptAge = 0

	// ldb=nil: safe because processed marker exits before any LLM/DB write.
	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if report.SkippedFile == 0 {
		t.Error("expected file to be skipped by processed marker")
	}
	if report.ProcessedFiles != 0 {
		t.Errorf("ProcessedFiles = %d, want 0", report.ProcessedFiles)
	}
	if store.markFileCalls != 0 {
		t.Errorf("markFileCalls = %d, want 0 (file was skipped)", store.markFileCalls)
	}
}

func TestRunner_force_flag_bypasses_processed_marker(t *testing.T) {
	dir := t.TempDir()
	writeJSONL(t, dir, [][]byte{
		sysInitLine("/home/user/project"),
		userLine("force reprocess this"),
		assistantLine("reply one"),
		userLine("another message"),
		assistantLine("reply two"),
	})

	store := newFakeStore()
	store.alwaysProcessed = true // file looks already processed

	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = true  // dry-run so no LLM calls
	cfg.Force = true   // --force: ignore processed marker
	cfg.MaxTranscriptAge = 0

	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	// With Force=true the file is reprocessed even though it looks done.
	if report.ProcessedFiles == 0 {
		t.Error("Force=true: expected file to be reprocessed despite processed marker")
	}
}

// ─── Runner: chunk dedup skips already-processed chunks ──────────────────────

func TestRunner_chunk_dedup_by_hash(t *testing.T) {
	dir := t.TempDir()
	path := writeJSONL(t, dir, [][]byte{
		sysInitLine("/home/user/project"),
		userLine("first message to process"),
		assistantLine("first reply with details"),
		userLine("second message too"),
		assistantLine("second reply too"),
	})

	// Parse and chunk the transcript to learn the chunk hashes up front.
	msgs, _, err := ParseTranscript(path)
	if err != nil {
		t.Fatalf("ParseTranscript: %v", err)
	}
	sess := Session{Ref: TranscriptRef{Path: path}, Messages: msgs}
	chunks := ChunkSession(sess, 6000)
	if len(chunks) == 0 {
		t.Fatal("expected at least one chunk")
	}

	// Pre-mark all chunks as processed.
	store := newFakeStore()
	for _, c := range chunks {
		store.MarkChunkProcessed(path, c.Hash, c.ChunkIndex)
	}
	priorChunkCalls := store.markChunkCalls

	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = false // non-dry-run to exercise processChunk
	cfg.Force = false
	cfg.MaxTranscriptAge = 0

	// ldb=nil: safe because all chunks are pre-processed and return before LLM call.
	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	// All chunks should be skipped by hash dedup.
	if report.ChunksSkipped != len(chunks) {
		t.Errorf("ChunksSkipped = %d, want %d", report.ChunksSkipped, len(chunks))
	}
	// No new chunk marks should be written.
	if store.markChunkCalls != priorChunkCalls {
		t.Errorf("markChunkCalls changed by %d, want 0 (all chunks already processed)",
			store.markChunkCalls-priorChunkCalls)
	}
	// No LLM calls for skipped chunks.
	if report.LLMCalls != 0 {
		t.Errorf("LLMCalls = %d, want 0 (all chunks pre-processed)", report.LLMCalls)
	}
}

// ─── Runner: MinSessionMsgs filter ───────────────────────────────────────────

func TestRunner_min_session_msgs_filter(t *testing.T) {
	dir := t.TempDir()
	// Only 2 ConvMessages (user + assistant), but MinSessionMsgs=4.
	writeJSONL(t, dir, [][]byte{
		sysInitLine("/home/user/project"),
		userLine("hi"),
		assistantLine("hello"),
	})

	store := newFakeStore()
	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = true
	cfg.MinSessionMsgs = 4
	cfg.MaxTranscriptAge = 0

	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if report.SkippedFile == 0 {
		t.Error("expected file to be skipped due to MinSessionMsgs")
	}
	if report.ProcessedFiles != 0 {
		t.Errorf("ProcessedFiles = %d, want 0", report.ProcessedFiles)
	}
}

// ─── Runner: worker session filter ───────────────────────────────────────────

func TestRunner_worker_session_cwd_skipped(t *testing.T) {
	dir := t.TempDir()
	// cwd points to an orchestrator worktree → isWorkerSession=true.
	writeJSONL(t, dir, [][]byte{
		sysInitLine("/home/alice/.alluka/worktrees/abc123/nanika"),
		userLine("phase output"),
		assistantLine("phase done"),
		userLine("another output"),
		assistantLine("another done"),
	})

	store := newFakeStore()
	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = true
	cfg.MaxTranscriptAge = 0

	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if report.SkippedFile == 0 {
		t.Error("expected worker session to be skipped")
	}
	if report.ProcessedFiles != 0 {
		t.Errorf("ProcessedFiles = %d, want 0 (worker session)", report.ProcessedFiles)
	}
}

func TestRunner_non_worker_session_processed(t *testing.T) {
	dir := t.TempDir()
	// Regular user project → should not be skipped as a worker session.
	writeJSONL(t, dir, [][]byte{
		sysInitLine("/home/user/myproject"),
		userLine("explain this"),
		assistantLine("sure, here is the explanation"),
		userLine("thank you"),
		assistantLine("you're welcome"),
	})

	store := newFakeStore()
	cfg := DefaultConfig()
	cfg.RootDir = dir
	cfg.DryRun = true
	cfg.MaxTranscriptAge = 0

	runner := NewRunner(store, nil, nil, cfg)
	report, err := runner.Run(context.Background(), "test", "")
	if err != nil {
		t.Fatalf("Run: %v", err)
	}

	if report.ProcessedFiles == 0 {
		t.Error("expected regular user session to be processed")
	}
}
