package zettel

// FindingZettel represents a FINDING: marker extracted from phase output and
// persisted as its own note with a backlink to the parent mission.
// Implementation pending T4.1 (TRK-529 Phase 4).
type FindingZettel struct {
	ID        string // sequential ID, e.g. "F001"
	MissionID string
	Content   string
}
