// Package health tracks per-source gather metrics (success/failure counts and
// latency) and persists them to ~/.scout/health.json.
package health

import (
	"encoding/json"
	"os"
	"path/filepath"
	"sync"
	"time"
)

// SourceHealth holds health metrics for a single gather source.
type SourceHealth struct {
	LastSuccess    *time.Time `json:"last_success,omitempty"`
	LastFailure    *time.Time `json:"last_failure,omitempty"`
	SuccessCount   int        `json:"success_count"`
	FailureCount   int        `json:"failure_count"`
	TotalLatencyMs int64      `json:"total_latency_ms"`
	CallCount      int        `json:"call_count"`
}

// AvgLatencyMs returns the average latency in milliseconds across all recorded
// calls, or 0 if no calls have been recorded.
func (s *SourceHealth) AvgLatencyMs() float64 {
	if s.CallCount == 0 {
		return 0
	}
	return float64(s.TotalLatencyMs) / float64(s.CallCount)
}

// Status returns a human-readable health status: "healthy", "degraded",
// "failing", or "unknown".
func (s *SourceHealth) Status() string {
	if s.SuccessCount == 0 && s.FailureCount == 0 {
		return "unknown"
	}
	if s.FailureCount == 0 {
		return "healthy"
	}
	if s.SuccessCount == 0 {
		return "failing"
	}
	// Degraded when failure rate exceeds 20%
	if s.FailureCount*5 > s.SuccessCount {
		return "degraded"
	}
	return "healthy"
}

// Store manages health data for all sources.
type Store struct {
	mu        sync.Mutex
	path      string
	Sources   map[string]*SourceHealth `json:"sources"`
	UpdatedAt time.Time                `json:"updated_at,omitempty"`
}

// Load reads health data from path, returning a fresh empty Store when the
// file does not yet exist.
func Load(path string) (*Store, error) {
	s := &Store{
		path:    path,
		Sources: make(map[string]*SourceHealth),
	}
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return s, nil
	}
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(data, s); err != nil {
		return nil, err
	}
	if s.Sources == nil {
		s.Sources = make(map[string]*SourceHealth)
	}
	s.path = path
	return s, nil
}

// Save persists the health data to the path set at load time.
func (s *Store) Save() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.path == "" {
		return nil
	}
	s.UpdatedAt = time.Now().UTC()
	if err := os.MkdirAll(filepath.Dir(s.path), 0700); err != nil {
		return err
	}
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(s.path, data, 0600)
}

// RecordSuccess records a successful gather call for source with its latency.
func (s *Store) RecordSuccess(source string, latency time.Duration) {
	s.mu.Lock()
	defer s.mu.Unlock()
	h := s.getOrCreate(source)
	now := time.Now().UTC()
	h.LastSuccess = &now
	h.SuccessCount++
	h.TotalLatencyMs += latency.Milliseconds()
	h.CallCount++
}

// RecordFailure records a failed gather call for source with its latency.
func (s *Store) RecordFailure(source string, latency time.Duration) {
	s.mu.Lock()
	defer s.mu.Unlock()
	h := s.getOrCreate(source)
	now := time.Now().UTC()
	h.LastFailure = &now
	h.FailureCount++
	h.TotalLatencyMs += latency.Milliseconds()
	h.CallCount++
}

func (s *Store) getOrCreate(source string) *SourceHealth {
	if _, ok := s.Sources[source]; !ok {
		s.Sources[source] = &SourceHealth{}
	}
	return s.Sources[source]
}
