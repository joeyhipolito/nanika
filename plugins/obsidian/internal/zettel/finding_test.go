package zettel

import "testing"

// T4.1 — §10.4 Phase 4
// Asserts: a phase with 3 FINDING: markers produces 3 separate findings/F*.md
// Zettels, each written atomically and each containing a backlink to the parent
// mission.
func TestFindingZettel_Atomicity(t *testing.T) {
	t.Skip("RED — T4.1 not yet implemented (blocks on TRK-529 Phase 4)")
}

// T4.2 — §10.4 Phase 4
// Asserts: when finding F001 is superseded by F007, both frontmatters contain
// bidirectional supersedes/superseded_by links and vault-doctor reports no
// invariant violations.
func TestSupersession_Bidirectional(t *testing.T) {
	t.Skip("RED — T4.2 not yet implemented (blocks on TRK-529 Phase 4)")
}
