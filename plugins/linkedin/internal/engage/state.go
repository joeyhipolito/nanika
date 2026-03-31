package engage

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/joeyhipolito/nanika-linkedin/internal/config"
)

const stateFile = "engaged.json"
const pruneAge = 30 * 24 * time.Hour

// Action constants for engagement records.
const (
	ActionComment = "comment"
	ActionReact   = "react"
	ActionFailed  = "failed"
)

// PostURL returns the LinkedIn feed URL for an activity URN.
func PostURL(activityURN string) string {
	return "https://www.linkedin.com/feed/update/" + activityURN + "/"
}

// EngagedEntry records a single engagement action.
type EngagedEntry struct {
	ActivityURN string `json:"activity_urn"`
	AuthorName  string `json:"author_name,omitempty"`
	Action      string `json:"action"` // ActionComment, ActionReact, ActionFailed
	URL         string `json:"url,omitempty"`
	Timestamp   string `json:"timestamp"`
}

// State tracks which posts have been engaged with.
type State struct {
	Engaged []EngagedEntry `json:"engaged"`
	path    string
	seen    map[string]bool // O(1) lookup cache; not serialized
}

// LoadState reads the engaged state from ~/.linkedin/engaged.json.
// Auto-prunes entries older than 30 days.
func LoadState() (*State, error) {
	dir, err := config.BaseDir()
	if err != nil {
		return nil, fmt.Errorf("finding config dir: %w", err)
	}
	path := filepath.Join(dir, stateFile)

	s := &State{path: path}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			s.seen = make(map[string]bool)
			return s, nil
		}
		// Corrupted file — start fresh
		fmt.Fprintf(os.Stderr, "warning: engaged.json corrupted, starting fresh: %v\n", err)
		s.seen = make(map[string]bool)
		return s, nil
	}

	if err := json.Unmarshal(data, s); err != nil {
		fmt.Fprintf(os.Stderr, "warning: engaged.json parse error, starting fresh: %v\n", err)
		return &State{path: path, seen: make(map[string]bool)}, nil
	}

	s.path = path
	s.prune()
	s.buildCache()
	return s, nil
}

// IsEngaged returns true if the given activity URN has already been engaged with.
func (s *State) IsEngaged(activityURN string) bool {
	return s.seen[activityURN]
}

// Record adds an engagement entry and saves to disk.
func (s *State) Record(activityURN, action, authorName string) error {
	s.Engaged = append(s.Engaged, EngagedEntry{
		ActivityURN: activityURN,
		AuthorName:  authorName,
		Action:      action,
		URL:         PostURL(activityURN),
		Timestamp:   time.Now().UTC().Format(time.RFC3339),
	})
	s.seen[activityURN] = true
	return s.save()
}

// prune removes entries older than 30 days.
func (s *State) prune() {
	cutoff := time.Now().Add(-pruneAge)
	var kept []EngagedEntry
	for _, e := range s.Engaged {
		t, err := time.Parse(time.RFC3339, e.Timestamp)
		if err != nil {
			kept = append(kept, e) // keep unparseable
			continue
		}
		if t.After(cutoff) {
			kept = append(kept, e)
		}
	}
	s.Engaged = kept
}

// buildCache populates the seen map from Engaged entries.
func (s *State) buildCache() {
	s.seen = make(map[string]bool, len(s.Engaged))
	for _, e := range s.Engaged {
		s.seen[e.ActivityURN] = true
	}
}

func (s *State) save() error {
	dir := filepath.Dir(s.path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("creating state dir: %w", err)
	}

	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling state: %w", err)
	}

	return os.WriteFile(s.path, data, 0600)
}
