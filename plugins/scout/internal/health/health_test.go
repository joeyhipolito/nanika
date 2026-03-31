package health

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestRecordSuccess(t *testing.T) {
	s := &Store{
		path:    filepath.Join(t.TempDir(), "health.json"),
		Sources: make(map[string]*SourceHealth),
	}

	s.RecordSuccess("hackernews", 300*time.Millisecond)

	h := s.Sources["hackernews"]
	if h == nil {
		t.Fatal("expected health entry for hackernews")
	}
	if h.SuccessCount != 1 {
		t.Errorf("SuccessCount = %d, want 1", h.SuccessCount)
	}
	if h.FailureCount != 0 {
		t.Errorf("FailureCount = %d, want 0", h.FailureCount)
	}
	if h.LastSuccess == nil {
		t.Error("expected LastSuccess to be set")
	}
	if h.TotalLatencyMs != 300 {
		t.Errorf("TotalLatencyMs = %d, want 300", h.TotalLatencyMs)
	}
	if h.CallCount != 1 {
		t.Errorf("CallCount = %d, want 1", h.CallCount)
	}
}

func TestRecordFailure(t *testing.T) {
	s := &Store{
		path:    filepath.Join(t.TempDir(), "health.json"),
		Sources: make(map[string]*SourceHealth),
	}

	s.RecordFailure("reddit", 150*time.Millisecond)

	h := s.Sources["reddit"]
	if h == nil {
		t.Fatal("expected health entry for reddit")
	}
	if h.FailureCount != 1 {
		t.Errorf("FailureCount = %d, want 1", h.FailureCount)
	}
	if h.SuccessCount != 0 {
		t.Errorf("SuccessCount = %d, want 0", h.SuccessCount)
	}
	if h.LastFailure == nil {
		t.Error("expected LastFailure to be set")
	}
	if h.TotalLatencyMs != 150 {
		t.Errorf("TotalLatencyMs = %d, want 150", h.TotalLatencyMs)
	}
}

func TestAvgLatencyMs(t *testing.T) {
	h := &SourceHealth{}
	if got := h.AvgLatencyMs(); got != 0 {
		t.Errorf("AvgLatencyMs() = %f, want 0 for empty entry", got)
	}

	h.TotalLatencyMs = 900
	h.CallCount = 3
	if got := h.AvgLatencyMs(); got != 300 {
		t.Errorf("AvgLatencyMs() = %f, want 300", got)
	}
}

func TestStatus(t *testing.T) {
	tests := []struct {
		name string
		h    SourceHealth
		want string
	}{
		{"no data", SourceHealth{}, "unknown"},
		{"only successes", SourceHealth{SuccessCount: 10}, "healthy"},
		{"only failures", SourceHealth{FailureCount: 3}, "failing"},
		{"low failure rate", SourceHealth{SuccessCount: 20, FailureCount: 1}, "healthy"},
		{"high failure rate", SourceHealth{SuccessCount: 2, FailureCount: 3}, "degraded"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.h.Status(); got != tc.want {
				t.Errorf("Status() = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestLoadSaveRoundtrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "health.json")

	s1, err := Load(path)
	if err != nil {
		t.Fatalf("Load (new): %v", err)
	}
	s1.RecordSuccess("hackernews", 300*time.Millisecond)
	s1.RecordSuccess("hackernews", 200*time.Millisecond)
	s1.RecordFailure("reddit", 100*time.Millisecond)

	if err := s1.Save(); err != nil {
		t.Fatalf("Save: %v", err)
	}

	s2, err := Load(path)
	if err != nil {
		t.Fatalf("Load (reload): %v", err)
	}

	hn := s2.Sources["hackernews"]
	if hn == nil {
		t.Fatal("expected hackernews entry after reload")
	}
	if hn.SuccessCount != 2 {
		t.Errorf("SuccessCount = %d, want 2", hn.SuccessCount)
	}
	if hn.TotalLatencyMs != 500 {
		t.Errorf("TotalLatencyMs = %d, want 500", hn.TotalLatencyMs)
	}
	if hn.CallCount != 2 {
		t.Errorf("CallCount = %d, want 2", hn.CallCount)
	}

	rr := s2.Sources["reddit"]
	if rr == nil {
		t.Fatal("expected reddit entry after reload")
	}
	if rr.FailureCount != 1 {
		t.Errorf("FailureCount = %d, want 1", rr.FailureCount)
	}
}

func TestLoad_NonexistentFile(t *testing.T) {
	s, err := Load("/nonexistent/path/health.json")
	if err != nil {
		t.Fatalf("expected no error for missing file, got: %v", err)
	}
	if len(s.Sources) != 0 {
		t.Errorf("expected empty Sources, got %d entries", len(s.Sources))
	}
}

func TestSave_CreatesFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "health.json")
	s, _ := Load(path)

	if err := s.Save(); err != nil {
		t.Fatalf("Save: %v", err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Errorf("expected file to exist after Save: %v", err)
	}
}

func TestSave_EmptyPath(t *testing.T) {
	s := &Store{Sources: make(map[string]*SourceHealth)}
	if err := s.Save(); err != nil {
		t.Errorf("Save with empty path should be a no-op, got: %v", err)
	}
}

func TestConcurrentAccess(t *testing.T) {
	s := &Store{
		path:    filepath.Join(t.TempDir(), "health.json"),
		Sources: make(map[string]*SourceHealth),
	}

	done := make(chan struct{})
	for i := 0; i < 10; i++ {
		go func(i int) {
			if i%2 == 0 {
				s.RecordSuccess("source", time.Duration(i)*time.Millisecond)
			} else {
				s.RecordFailure("source", time.Duration(i)*time.Millisecond)
			}
			done <- struct{}{}
		}(i)
	}
	for i := 0; i < 10; i++ {
		<-done
	}

	h := s.Sources["source"]
	if h == nil {
		t.Fatal("expected health entry after concurrent access")
	}
	if h.SuccessCount+h.FailureCount != 10 {
		t.Errorf("expected 10 total calls, got %d", h.SuccessCount+h.FailureCount)
	}
}
