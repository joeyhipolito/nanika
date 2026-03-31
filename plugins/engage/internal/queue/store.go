// Package queue manages the draft review queue for the engage CLI.
// Drafts are stored as JSON files in ~/.alluka/engage/queue/<id>.json.
// States: pending -> approved or rejected; approved -> posted.
package queue

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-engage/internal/enrich"
)

// State represents a draft's lifecycle stage.
type State string

const (
	StatePending  State = "pending"
	StateApproved State = "approved"
	StateRejected State = "rejected"
	StatePosted   State = "posted"
)

// Draft is a generated comment draft awaiting human review.
type Draft struct {
	ID          string                  `json:"id"`
	State       State                   `json:"state"`
	Platform    string                  `json:"platform"`
	Opportunity enrich.EnrichedOpportunity `json:"opportunity"`
	Comment     string                  `json:"comment"`
	Persona     string                  `json:"persona"`
	CreatedAt   time.Time               `json:"created_at"`
	ReviewedAt  *time.Time              `json:"reviewed_at,omitempty"`
	PostedAt    *time.Time              `json:"posted_at,omitempty"`
	Note        string                  `json:"note,omitempty"` // reviewer note on reject
}

// Store manages draft JSON files on disk.
type Store struct {
	dir string
}

// NewStore returns a Store backed by the given directory (created if absent).
func NewStore(dir string) (*Store, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("creating queue dir %s: %w", dir, err)
	}
	return &Store{dir: dir}, nil
}

// DefaultDir returns the default queue directory, respecting ALLUKA_HOME.
func DefaultDir() string {
	home := os.Getenv("ALLUKA_HOME")
	if home == "" {
		home = filepath.Join(os.Getenv("HOME"), ".alluka")
	}
	return filepath.Join(home, "engage", "queue")
}

// Save writes a draft to disk. Creates or overwrites the file for draft.ID.
func (s *Store) Save(d *Draft) error {
	if d.ID == "" {
		return fmt.Errorf("draft ID is required")
	}
	data, err := json.MarshalIndent(d, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling draft %s: %w", d.ID, err)
	}
	path := s.path(d.ID)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return fmt.Errorf("writing draft %s: %w", d.ID, err)
	}
	return nil
}

// Load reads a single draft by ID.
func (s *Store) Load(id string) (*Draft, error) {
	data, err := os.ReadFile(s.path(id))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("draft %q not found", id)
		}
		return nil, fmt.Errorf("reading draft %s: %w", id, err)
	}
	var d Draft
	if err := json.Unmarshal(data, &d); err != nil {
		return nil, fmt.Errorf("parsing draft %s: %w", id, err)
	}
	return &d, nil
}

// List returns all drafts, optionally filtered by state. Pass "" to list all.
func (s *Store) List(state State) ([]*Draft, error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading queue dir: %w", err)
	}

	var drafts []*Draft
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".json") {
			continue
		}
		id := strings.TrimSuffix(e.Name(), ".json")
		d, err := s.Load(id)
		if err != nil {
			fmt.Fprintf(os.Stderr, "warn: skipping %s: %v\n", e.Name(), err)
			continue
		}
		if state == "" || d.State == state {
			drafts = append(drafts, d)
		}
	}
	return drafts, nil
}

// Transition moves a draft to a new state and persists the change.
// Returns an error if the transition is invalid.
func (s *Store) Transition(id string, newState State, note string) (*Draft, error) {
	d, err := s.Load(id)
	if err != nil {
		return nil, err
	}

	switch {
	case d.State == StatePending && (newState == StateApproved || newState == StateRejected):
		// valid
	case d.State == StateApproved && newState == StatePosted:
		// valid
	default:
		return nil, fmt.Errorf("cannot transition %s from %s to %s", id, d.State, newState)
	}

	now := time.Now()
	d.State = newState
	d.Note = note

	if newState == StateApproved || newState == StateRejected {
		d.ReviewedAt = &now
	}
	if newState == StatePosted {
		d.PostedAt = &now
	}

	if err := s.Save(d); err != nil {
		return nil, err
	}
	return d, nil
}

// GenerateID returns a short ID derived from platform, opportunity ID, and time.
func GenerateID(platform, oppID string) string {
	ts := time.Now().UTC().Format("20060102-150405")
	safe := strings.Map(func(r rune) rune {
		if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') || r == '-' {
			return r
		}
		return '-'
	}, oppID)
	// Keep ID short: platform-oppid-timestamp
	if len(safe) > 12 {
		safe = safe[:12]
	}
	return fmt.Sprintf("%s-%s-%s", platform, safe, ts)
}

func (s *Store) path(id string) string {
	return filepath.Join(s.dir, id+".json")
}
