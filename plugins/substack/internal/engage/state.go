package engage

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

const stateFile = "engaged.json"
const pruneAge = 30 * 24 * time.Hour

// NoteURL returns the Substack URL for a note.
func NoteURL(noteID int) string {
	return fmt.Sprintf("https://substack.com/note/c-%d", noteID)
}

// EngagedEntry records a single engagement action.
type EngagedEntry struct {
	NoteID    int    `json:"note_id"`
	Author    string `json:"author,omitempty"`
	Action    string `json:"action"` // "comment", "react", "failed"
	URL       string `json:"url,omitempty"`
	Timestamp string `json:"timestamp"`
}

// State tracks which notes have been engaged with.
type State struct {
	Engaged []EngagedEntry `json:"engaged"`
	path    string
}

// LoadState reads the engaged state from ~/.substack/engaged.json.
// Auto-prunes entries older than 30 days.
func LoadState() (*State, error) {
	dir, err := substackDir()
	if err != nil {
		return nil, err
	}
	path := filepath.Join(dir, stateFile)

	s := &State{path: path}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return s, nil
		}
		// Corrupted file — start fresh
		fmt.Fprintf(os.Stderr, "warning: engaged.json corrupted, starting fresh: %v\n", err)
		return s, nil
	}

	if err := json.Unmarshal(data, s); err != nil {
		fmt.Fprintf(os.Stderr, "warning: engaged.json parse error, starting fresh: %v\n", err)
		return &State{path: path}, nil
	}

	s.path = path
	s.prune()
	return s, nil
}

// IsEngaged returns true if the given note ID has already been engaged with.
func (s *State) IsEngaged(noteID int) bool {
	for _, e := range s.Engaged {
		if e.NoteID == noteID {
			return true
		}
	}
	return false
}

// Record adds an engagement entry and saves to disk.
func (s *State) Record(noteID int, action, author string) error {
	s.Engaged = append(s.Engaged, EngagedEntry{
		NoteID:    noteID,
		Author:    author,
		Action:    action,
		URL:       NoteURL(noteID),
		Timestamp: time.Now().UTC().Format(time.RFC3339),
	})
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

func substackDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("finding home dir: %w", err)
	}
	return filepath.Join(home, ".substack"), nil
}
