// Package ingest handles importing data from external sources (scout, learnings)
// into the Obsidian vault as structured notes.
package ingest

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

const stateFile = ".obsidian/ingest-state.json"

// State tracks which items have already been ingested to avoid re-processing.
type State struct {
	// Scout maps "topic/file_id" -> bool (ingested)
	Scout map[string]bool `json:"scout"`
	// Learnings maps learning ID -> bool (ingested)
	Learnings map[string]bool `json:"learnings"`
	// Workspaces maps workspace ID -> bool (captured via auto-capture)
	Workspaces map[string]bool `json:"workspaces"`
}

// statePath returns the full path to the ingest state file.
func statePath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("cannot determine home directory: %w", err)
	}
	return filepath.Join(home, stateFile), nil
}

// LoadState reads the ingest state from disk. Returns an empty state if the file doesn't exist.
func LoadState() (*State, error) {
	path, err := statePath()
	if err != nil {
		return nil, err
	}

	s := &State{
		Scout:      make(map[string]bool),
		Learnings:  make(map[string]bool),
		Workspaces: make(map[string]bool),
	}

	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return s, nil
		}
		return nil, fmt.Errorf("cannot read ingest state: %w", err)
	}

	if err := json.Unmarshal(data, s); err != nil {
		// Corrupt state — start fresh
		return &State{
			Scout:      make(map[string]bool),
			Learnings:  make(map[string]bool),
			Workspaces: make(map[string]bool),
		}, nil
	}

	if s.Scout == nil {
		s.Scout = make(map[string]bool)
	}
	if s.Learnings == nil {
		s.Learnings = make(map[string]bool)
	}
	if s.Workspaces == nil {
		s.Workspaces = make(map[string]bool)
	}

	return s, nil
}

// Save writes the ingest state to disk.
func (s *State) Save() error {
	path, err := statePath()
	if err != nil {
		return err
	}

	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("cannot create state directory: %w", err)
	}

	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return fmt.Errorf("cannot marshal state: %w", err)
	}

	if err := os.WriteFile(path, data, 0600); err != nil {
		return fmt.Errorf("cannot write ingest state: %w", err)
	}

	return nil
}

// MarkScout marks a scout item as ingested.
func (s *State) MarkScout(key string) {
	s.Scout[key] = true
}

// MarkLearning marks a learning as ingested.
func (s *State) MarkLearning(id string) {
	s.Learnings[id] = true
}

// HasScout returns true if the scout item was already ingested.
func (s *State) HasScout(key string) bool {
	return s.Scout[key]
}

// HasLearning returns true if the learning was already ingested.
func (s *State) HasLearning(id string) bool {
	return s.Learnings[id]
}

// MarkWorkspace marks a workspace as captured.
func (s *State) MarkWorkspace(id string) {
	s.Workspaces[id] = true
}

// HasWorkspace returns true if the workspace was already captured.
func (s *State) HasWorkspace(id string) bool {
	return s.Workspaces[id]
}
