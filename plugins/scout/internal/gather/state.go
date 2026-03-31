package gather

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// GatherState tracks the last successful gather timestamp per topic per source.
// Stored at ~/.scout/gather-state.json.
type GatherState struct {
	// Topics maps topic name → source name → last gathered timestamp (UTC).
	Topics map[string]map[string]time.Time `json:"topics"`
}

// LoadState reads the gather state file from path.
// Returns an empty state (not an error) if the file is missing or corrupt.
func LoadState(path string) *GatherState {
	s := &GatherState{Topics: make(map[string]map[string]time.Time)}
	if path == "" {
		return s
	}

	data, err := os.ReadFile(path)
	if err != nil {
		// Missing file is normal on first run.
		return s
	}

	var loaded GatherState
	if err := json.Unmarshal(data, &loaded); err != nil {
		// Corrupt file — treat as fresh state.
		return s
	}

	if loaded.Topics != nil {
		s.Topics = loaded.Topics
	}
	return s
}

// SaveState writes the gather state to path atomically (temp file + rename).
func SaveState(path string, s *GatherState) error {
	if path == "" {
		return fmt.Errorf("gather state path is empty")
	}

	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("failed to create state directory: %w", err)
	}

	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal gather state: %w", err)
	}

	// Atomic write: write to temp file, then rename.
	tmpPath := path + ".tmp"
	if err := os.WriteFile(tmpPath, data, 0600); err != nil {
		return fmt.Errorf("failed to write temp state file: %w", err)
	}
	if err := os.Rename(tmpPath, path); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to rename state file: %w", err)
	}
	return nil
}

// Cutoff returns the last gather timestamp for the given topic/source pair.
// Returns a zero time.Time if no record exists (triggers full gather).
func (s *GatherState) Cutoff(topic, source string) time.Time {
	if s == nil || s.Topics == nil {
		return time.Time{}
	}
	srcs, ok := s.Topics[topic]
	if !ok {
		return time.Time{}
	}
	return srcs[source]
}

// SetCutoff records the last gather timestamp for a topic/source pair.
func (s *GatherState) SetCutoff(topic, source string, ts time.Time) {
	if s.Topics == nil {
		s.Topics = make(map[string]map[string]time.Time)
	}
	if _, ok := s.Topics[topic]; !ok {
		s.Topics[topic] = make(map[string]time.Time)
	}
	s.Topics[topic][source] = ts
}

// FilterAfter returns items with Timestamp strictly after cutoff.
// If cutoff is zero, all items are returned unchanged.
func FilterAfter(items []IntelItem, cutoff time.Time) []IntelItem {
	if cutoff.IsZero() {
		return items
	}
	var out []IntelItem
	for _, item := range items {
		if item.Timestamp.After(cutoff) {
			out = append(out, item)
		}
	}
	return out
}
