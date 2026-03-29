package claims

import (
	"path/filepath"
	"testing"
	"time"
)

func openTestDB(t *testing.T) *DB {
	t.Helper()
	db, err := OpenDB(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

func TestClaimReleaseCycle(t *testing.T) {
	db := openTestDB(t)
	files := []string{"main.go", "internal/foo/bar.go"}

	if err := db.ClaimFiles("mission-1", "/repo", files); err != nil {
		t.Fatalf("ClaimFiles: %v", err)
	}

	// Same mission sees no conflicts with itself.
	conflicts, err := db.CheckConflicts("mission-1", "/repo", files)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected no self-conflicts, got %d", len(conflicts))
	}

	// A different mission should see both files as conflicted.
	conflicts, err = db.CheckConflicts("mission-2", "/repo", files)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 2 {
		t.Errorf("expected 2 conflicts, got %d", len(conflicts))
	}

	// Release mission-1's claims.
	if err := db.ReleaseAll("mission-1"); err != nil {
		t.Fatal(err)
	}

	// Now mission-2 sees no conflicts.
	conflicts, err = db.CheckConflicts("mission-2", "/repo", files)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected 0 conflicts after release, got %d", len(conflicts))
	}
}

func TestConflictDetection(t *testing.T) {
	db := openTestDB(t)
	files := []string{"a.go", "b.go", "c.go"}

	// mission-1 claims a.go and b.go only.
	if err := db.ClaimFiles("mission-1", "/repo", files[:2]); err != nil {
		t.Fatal(err)
	}

	// mission-2 wants all three — only a.go and b.go should conflict.
	conflicts, err := db.CheckConflicts("mission-2", "/repo", files)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 2 {
		t.Errorf("expected 2 conflicts (a.go, b.go), got %d", len(conflicts))
	}
	for _, c := range conflicts {
		if c.MissionID != "mission-1" {
			t.Errorf("expected mission-1, got %q", c.MissionID)
		}
	}
}

func TestStaleClaims(t *testing.T) {
	db := openTestDB(t)

	// Insert an artificially old claim directly.
	_, err := db.db.Exec(`
		INSERT INTO file_claims (file_path, mission_id, repo_root, claimed_at, released_at)
		VALUES ('old.go', 'old-mission', '/repo', '2020-01-01T00:00:00Z', NULL)
	`)
	if err != nil {
		t.Fatal(err)
	}

	n, err := db.PurgeStaleClaims(7 * 24 * time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	if n != 1 {
		t.Errorf("expected 1 purged, got %d", n)
	}

	// A recent claim should not be purged.
	if err := db.ClaimFiles("new-mission", "/repo", []string{"new.go"}); err != nil {
		t.Fatal(err)
	}
	n, err = db.PurgeStaleClaims(7 * 24 * time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Errorf("expected 0 purged for recent claim, got %d", n)
	}
}

func TestDifferentRepos(t *testing.T) {
	db := openTestDB(t)

	if err := db.ClaimFiles("mission-1", "/repo-a", []string{"main.go"}); err != nil {
		t.Fatal(err)
	}

	// Same file path but different repo — no conflict expected.
	conflicts, err := db.CheckConflicts("mission-2", "/repo-b", []string{"main.go"})
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected 0 conflicts across different repos, got %d", len(conflicts))
	}
}

func TestReleaseIdempotent(t *testing.T) {
	db := openTestDB(t)

	if err := db.ClaimFiles("m1", "/repo", []string{"x.go"}); err != nil {
		t.Fatal(err)
	}
	if err := db.ReleaseAll("m1"); err != nil {
		t.Fatal(err)
	}
	// Second release should be a no-op, not an error.
	if err := db.ReleaseAll("m1"); err != nil {
		t.Fatalf("second ReleaseAll: %v", err)
	}
}

func TestClaimFilesEmpty(t *testing.T) {
	db := openTestDB(t)
	// Should be a no-op, not an error.
	if err := db.ClaimFiles("m1", "/repo", nil); err != nil {
		t.Errorf("ClaimFiles(nil): %v", err)
	}
	conflicts, err := db.CheckConflicts("m2", "/repo", nil)
	if err != nil {
		t.Errorf("CheckConflicts(nil): %v", err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected no conflicts for empty file list")
	}
}

func TestUpdateFileClaimsWithFiles(t *testing.T) {
	db := openTestDB(t)

	// Initial repo-root marker claim.
	if err := db.ClaimFiles("m1", "/repo", []string{"."}); err != nil {
		t.Fatal(err)
	}

	// Replace with per-file claims — stale "." must be released.
	updated := []string{"a.go", "b.go"}
	if err := db.UpdateFileClaimsWithFiles("m1", "/repo", updated); err != nil {
		t.Fatalf("UpdateFileClaimsWithFiles: %v", err)
	}

	// "." must no longer conflict — it was released.
	conflicts, err := db.CheckConflicts("m2", "/repo", []string{"."})
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected stale '.' claim released, got %d conflict(s)", len(conflicts))
	}

	// a.go and b.go must now conflict for m2.
	conflicts, err = db.CheckConflicts("m2", "/repo", updated)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 2 {
		t.Errorf("expected 2 per-file conflicts, got %d", len(conflicts))
	}

	// Calling with an empty file list must release all active claims.
	if err := db.UpdateFileClaimsWithFiles("m1", "/repo", nil); err != nil {
		t.Fatalf("UpdateFileClaimsWithFiles(nil): %v", err)
	}
	conflicts, err = db.CheckConflicts("m2", "/repo", updated)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected 0 conflicts after empty update, got %d", len(conflicts))
	}
}

// TestLifecycleFailureRetainsClaims proves that a mission whose run ends in
// failure (no ReleaseAll call) keeps its claims active so that a parallel
// mission can still detect the conflict.
func TestLifecycleFailureRetainsClaims(t *testing.T) {
	db := openTestDB(t)
	files := []string{"pkg/foo.go", "pkg/bar.go"}

	// Mission A starts and claims files (simulating first-run start).
	if err := db.ClaimFiles("mission-A", "/repo", files); err != nil {
		t.Fatalf("ClaimFiles: %v", err)
	}

	// Mission A fails — no ReleaseAll is called. Mission B should still see conflicts.
	conflicts, err := db.CheckConflicts("mission-B", "/repo", files)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 2 {
		t.Errorf("expected 2 conflicts after failure (claims retained), got %d", len(conflicts))
	}
	for _, c := range conflicts {
		if c.MissionID != "mission-A" {
			t.Errorf("conflict attributed to wrong mission: %q", c.MissionID)
		}
	}
}

// TestLifecycleResumeSuccessReleases proves the resume-to-success path:
// a previously-failed mission (claims still active) resumes, updates its
// per-file claims, succeeds, and then releases — leaving no active conflicts.
func TestLifecycleResumeSuccessReleases(t *testing.T) {
	db := openTestDB(t)

	// First run: claim repo-root marker "." and then fail (no release).
	if err := db.ClaimFiles("mission-A", "/repo", []string{"."}); err != nil {
		t.Fatalf("ClaimFiles (first run): %v", err)
	}

	// Resume: update to real per-file claims (mirrors updatePerFileClaimsPostExecution).
	resumeFiles := []string{"cmd/main.go", "internal/engine/engine.go"}
	if err := db.UpdateFileClaimsWithFiles("mission-A", "/repo", resumeFiles); err != nil {
		t.Fatalf("UpdateFileClaimsWithFiles (resume): %v", err)
	}

	// Stale "." marker must be gone.
	conflicts, err := db.CheckConflicts("mission-B", "/repo", []string{"."})
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected stale '.' claim released on resume, got %d conflict(s)", len(conflicts))
	}

	// Per-file claims must be active.
	conflicts, err = db.CheckConflicts("mission-B", "/repo", resumeFiles)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 2 {
		t.Errorf("expected 2 active per-file conflicts during resume, got %d", len(conflicts))
	}

	// Success: release all claims (mirrors the success branch in run.go).
	if err := db.ReleaseAll("mission-A"); err != nil {
		t.Fatalf("ReleaseAll (success): %v", err)
	}

	// No conflicts remain after successful completion.
	conflicts, err = db.CheckConflicts("mission-B", "/repo", resumeFiles)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 0 {
		t.Errorf("expected 0 conflicts after successful release, got %d", len(conflicts))
	}
}

// TestLifecycleRepeatedUpdates proves that calling UpdateFileClaimsWithFiles
// multiple times converges to only the latest file set being active, with no
// stale rows leaking through from prior iterations.
func TestLifecycleRepeatedUpdates(t *testing.T) {
	db := openTestDB(t)

	sets := [][]string{
		{"a.go", "b.go", "c.go"},
		{"b.go", "d.go"},
		{"e.go"},
	}

	for i, files := range sets {
		if err := db.UpdateFileClaimsWithFiles("mission-A", "/repo", files); err != nil {
			t.Fatalf("UpdateFileClaimsWithFiles (iteration %d): %v", i, err)
		}

		// Only the current set should conflict; all previous files must be released.
		conflicts, err := db.CheckConflicts("mission-B", "/repo", files)
		if err != nil {
			t.Fatal(err)
		}
		if len(conflicts) != len(files) {
			t.Errorf("iteration %d: expected %d conflicts, got %d", i, len(files), len(conflicts))
		}

		// All files from previous iterations must no longer conflict.
		if i > 0 {
			prev := sets[i-1]
			old, err := db.CheckConflicts("mission-B", "/repo", prev)
			if err != nil {
				t.Fatal(err)
			}
			// Only files that appear in both prev AND current set should still conflict.
			var stillActive int
			for _, f := range prev {
				for _, cf := range files {
					if f == cf {
						stillActive++
						break
					}
				}
			}
			if len(old) != stillActive {
				t.Errorf("iteration %d: expected %d overlap conflicts from prev set, got %d", i, stillActive, len(old))
			}
		}
	}

	// Final state: only "e.go" is active.
	all := []string{"a.go", "b.go", "c.go", "d.go", "e.go"}
	conflicts, err := db.CheckConflicts("mission-B", "/repo", all)
	if err != nil {
		t.Fatal(err)
	}
	if len(conflicts) != 1 || conflicts[0].FilePath != "e.go" {
		t.Errorf("expected only e.go active at end, got %v", conflicts)
	}
}
