package gather

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

// ---- GatherState helpers ----

func TestGatherState_CutoffMissing(t *testing.T) {
	s := &GatherState{Topics: make(map[string]map[string]time.Time)}
	got := s.Cutoff("ai", "hackernews")
	if !got.IsZero() {
		t.Errorf("expected zero time for missing entry, got %v", got)
	}
}

func TestGatherState_SetAndGetCutoff(t *testing.T) {
	s := &GatherState{Topics: make(map[string]map[string]time.Time)}
	ts := time.Date(2026, 3, 18, 12, 0, 0, 0, time.UTC)

	s.SetCutoff("ai", "hackernews", ts)

	got := s.Cutoff("ai", "hackernews")
	if !got.Equal(ts) {
		t.Errorf("expected %v, got %v", ts, got)
	}
}

func TestGatherState_NilState(t *testing.T) {
	var s *GatherState
	got := s.Cutoff("ai", "hackernews")
	if !got.IsZero() {
		t.Errorf("expected zero time for nil state, got %v", got)
	}
}

// ---- LoadState ----

func TestLoadState_MissingFile(t *testing.T) {
	s := LoadState("/nonexistent/path/state.json")
	if s == nil {
		t.Fatal("expected non-nil state")
	}
	if len(s.Topics) != 0 {
		t.Errorf("expected empty topics, got %v", s.Topics)
	}
}

func TestLoadState_CorruptFile(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "gather-state.json")
	if err := os.WriteFile(path, []byte("not json {{{{"), 0600); err != nil {
		t.Fatal(err)
	}
	s := LoadState(path)
	if s == nil || len(s.Topics) != 0 {
		t.Errorf("expected empty state from corrupt file, got %v", s)
	}
}

func TestLoadState_ValidFile(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "gather-state.json")

	ts := time.Date(2026, 3, 18, 10, 30, 0, 0, time.UTC)
	original := &GatherState{Topics: map[string]map[string]time.Time{
		"ai": {"hackernews": ts, "rss": ts.Add(time.Hour)},
	}}

	if err := SaveState(path, original); err != nil {
		t.Fatal(err)
	}

	loaded := LoadState(path)
	got := loaded.Cutoff("ai", "hackernews")
	if !got.Equal(ts) {
		t.Errorf("expected %v, got %v", ts, got)
	}
}

// ---- SaveState ----

func TestSaveState_AtomicWrite(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "gather-state.json")

	s := &GatherState{Topics: map[string]map[string]time.Time{
		"golang": {"github": time.Now().UTC()},
	}}
	if err := SaveState(path, s); err != nil {
		t.Fatalf("SaveState failed: %v", err)
	}

	// Temp file must not exist after successful save.
	if _, err := os.Stat(path + ".tmp"); !os.IsNotExist(err) {
		t.Error("expected tmp file to be cleaned up after rename")
	}

	// The real file must exist.
	if _, err := os.Stat(path); err != nil {
		t.Errorf("expected state file to exist: %v", err)
	}
}

func TestSaveState_EmptyPath(t *testing.T) {
	s := &GatherState{}
	if err := SaveState("", s); err == nil {
		t.Error("expected error for empty path")
	}
}

func TestSaveState_RoundTrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "gather-state.json")

	ts1 := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	ts2 := time.Date(2026, 2, 15, 8, 30, 0, 0, time.UTC)

	s := &GatherState{Topics: map[string]map[string]time.Time{
		"topic-a": {"src1": ts1, "src2": ts2},
		"topic-b": {"src1": ts1},
	}}

	if err := SaveState(path, s); err != nil {
		t.Fatal(err)
	}

	loaded := LoadState(path)
	cases := []struct{ topic, src string; want time.Time }{
		{"topic-a", "src1", ts1},
		{"topic-a", "src2", ts2},
		{"topic-b", "src1", ts1},
	}
	for _, c := range cases {
		got := loaded.Cutoff(c.topic, c.src)
		if !got.Equal(c.want) {
			t.Errorf("Cutoff(%q, %q): want %v, got %v", c.topic, c.src, c.want, got)
		}
	}
}

// ---- FilterAfter ----

func makeItem(title string, ts time.Time) IntelItem {
	return IntelItem{ID: title, Title: title, Timestamp: ts}
}

func TestFilterAfter_ZeroCutoff(t *testing.T) {
	items := []IntelItem{
		makeItem("a", time.Now().Add(-24*time.Hour)),
		makeItem("b", time.Now()),
	}
	got := FilterAfter(items, time.Time{})
	if len(got) != len(items) {
		t.Errorf("zero cutoff: expected %d items, got %d", len(items), len(got))
	}
}

func TestFilterAfter_FiltersOldItems(t *testing.T) {
	cutoff := time.Date(2026, 3, 10, 0, 0, 0, 0, time.UTC)
	items := []IntelItem{
		makeItem("old", time.Date(2026, 3, 9, 23, 59, 59, 0, time.UTC)),
		makeItem("at-cutoff", cutoff),
		makeItem("new", time.Date(2026, 3, 11, 0, 0, 0, 0, time.UTC)),
	}
	got := FilterAfter(items, cutoff)
	if len(got) != 1 {
		t.Fatalf("expected 1 item after cutoff, got %d: %v", len(got), got)
	}
	if got[0].Title != "new" {
		t.Errorf("expected 'new', got %q", got[0].Title)
	}
}

func TestFilterAfter_AllOld(t *testing.T) {
	cutoff := time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)
	items := []IntelItem{
		makeItem("a", time.Date(2026, 3, 1, 0, 0, 0, 0, time.UTC)),
		makeItem("b", time.Date(2026, 3, 5, 0, 0, 0, 0, time.UTC)),
	}
	got := FilterAfter(items, cutoff)
	if len(got) != 0 {
		t.Errorf("expected 0 items, got %d", len(got))
	}
}

func TestFilterAfter_AllNew(t *testing.T) {
	cutoff := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	items := []IntelItem{
		makeItem("a", time.Date(2026, 3, 1, 0, 0, 0, 0, time.UTC)),
		makeItem("b", time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)),
	}
	got := FilterAfter(items, cutoff)
	if len(got) != 2 {
		t.Errorf("expected 2 items, got %d", len(got))
	}
}

func TestFilterAfter_EmptySlice(t *testing.T) {
	got := FilterAfter(nil, time.Now())
	if len(got) != 0 {
		t.Errorf("expected 0 items from nil input, got %d", len(got))
	}
}
