// Package history tracks all posted engagements for later reporting.
// Records are stored as JSON files in ~/.alluka/engage/history/<id>.json.
package history

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// Record captures a single posted comment engagement.
type Record struct {
	ID        string     `json:"id"`
	Platform  string     `json:"platform"`
	PostURL   string     `json:"post_url"`
	Comment   string     `json:"comment"`
	PostedAt  time.Time  `json:"posted_at"`
	// Engagement received — populated later via a fetch pass.
	Likes     int        `json:"likes,omitempty"`
	Replies   int        `json:"replies,omitempty"`
	FetchedAt *time.Time `json:"fetched_at,omitempty"`
}

// Store manages history JSON files on disk.
type Store struct {
	dir string
}

// NewStore returns a Store backed by the given directory (created if absent).
func NewStore(dir string) (*Store, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("creating history dir %s: %w", dir, err)
	}
	return &Store{dir: dir}, nil
}

// DefaultDir returns the default history directory, respecting ALLUKA_HOME.
func DefaultDir() string {
	home := os.Getenv("ALLUKA_HOME")
	if home == "" {
		home = filepath.Join(os.Getenv("HOME"), ".alluka")
	}
	return filepath.Join(home, "engage", "history")
}

// Save writes a record to disk, creating or overwriting the file for r.ID.
func (s *Store) Save(r *Record) error {
	if r.ID == "" {
		return fmt.Errorf("record ID is required")
	}
	data, err := json.MarshalIndent(r, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling record %s: %w", r.ID, err)
	}
	if err := os.WriteFile(filepath.Join(s.dir, r.ID+".json"), data, 0o644); err != nil {
		return fmt.Errorf("writing record %s: %w", r.ID, err)
	}
	return nil
}

// List returns all records sorted by posted_at descending.
// If since is non-zero, only records posted at or after since are returned.
func (s *Store) List(since time.Time) ([]*Record, error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading history dir: %w", err)
	}

	var records []*Record
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".json") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(s.dir, e.Name()))
		if err != nil {
			fmt.Fprintf(os.Stderr, "warn: skipping %s: %v\n", e.Name(), err)
			continue
		}
		var r Record
		if err := json.Unmarshal(data, &r); err != nil {
			fmt.Fprintf(os.Stderr, "warn: skipping %s: %v\n", e.Name(), err)
			continue
		}
		if !since.IsZero() && r.PostedAt.Before(since) {
			continue
		}
		records = append(records, &r)
	}

	sort.Slice(records, func(i, j int) bool {
		return records[i].PostedAt.After(records[j].PostedAt)
	})
	return records, nil
}

// GenerateID returns a unique ID for a history record derived from the draft ID.
func GenerateID(platform, draftID string) string {
	ts := time.Now().UTC().Format("20060102-150405")
	safe := strings.Map(func(r rune) rune {
		if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') || r == '-' {
			return r
		}
		return '-'
	}, draftID)
	if len(safe) > 20 {
		safe = safe[:20]
	}
	return fmt.Sprintf("hist-%s-%s-%s", platform, safe, ts)
}
