package learning

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

func newTestDB(t *testing.T) *DB {
	t.Helper()
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatalf("open in-memory db: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	ldb := &DB{db: db}
	if err := ldb.initSchema(); err != nil {
		t.Fatalf("initSchema: %v", err)
	}
	return ldb
}

func insertLearning(t *testing.T, db *DB, l Learning) {
	t.Helper()
	tags := strings.Join(l.Tags, ",")
	createdAt := l.CreatedAt
	if createdAt.IsZero() {
		createdAt = time.Now()
	}
	seenCount := l.SeenCount
	if seenCount < 1 {
		seenCount = 1
	}
	var embBlob []byte
	if l.Embedding != nil {
		embBlob = EncodeEmbedding(l.Embedding)
	}
	_, err := db.db.Exec(`
		INSERT INTO learnings (
			id, type, content, context, domain, worker_name, workspace_id,
			tags, seen_count, used_count, quality_score, created_at, last_used_at, embedding,
			injection_count, compliance_count, compliance_rate, archived
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	`,
		l.ID, string(l.Type), l.Content, l.Context, l.Domain,
		l.WorkerName, l.WorkspaceID,
		tags, seenCount, l.UsageCount, l.QualityScore,
		createdAt.UTC().Format(time.RFC3339),
		formatNullableTime(l.LastUsedAt),
		embBlob,
		l.InjectionCount, l.ComplianceCount, l.ComplianceRate, 0,
	)
	if err != nil {
		t.Fatalf("insertLearning(%s): %v", l.ID, err)
	}
}

func setArchived(t *testing.T, db *DB, id string, archived int) {
	t.Helper()
	if _, err := db.db.Exec("UPDATE learnings SET archived = ? WHERE id = ?", archived, id); err != nil {
		t.Fatalf("setArchived(%s, %d): %v", id, archived, err)
	}
}

func getQualityScore(t *testing.T, db *DB, id string) float64 {
	t.Helper()
	var score float64
	if err := db.db.QueryRow("SELECT quality_score FROM learnings WHERE id = ?", id).Scan(&score); err != nil {
		t.Fatalf("getQualityScore(%s): %v", id, err)
	}
	return score
}

func getArchivedFlag(t *testing.T, db *DB, id string) int {
	t.Helper()
	var archived int
	if err := db.db.QueryRow("SELECT archived FROM learnings WHERE id = ?", id).Scan(&archived); err != nil {
		t.Fatalf("getArchivedFlag(%s): %v", id, err)
	}
	return archived
}

// ---------------------------------------------------------------------------
// TestArchiveDeadWeight
// ---------------------------------------------------------------------------

func TestArchiveDeadWeight(t *testing.T) {
	ctx := context.Background()
	ago := func(days int) time.Time {
		return time.Now().Add(-time.Duration(days) * 24 * time.Hour)
	}

	t.Run("criterion 1: never injected older than 90 days", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:             "c1-match",
			Type:           TypeInsight,
			Content:        "never injected old learning",
			Domain:         "dev",
			InjectionCount: 0,
			UsageCount:     0,
			QualityScore:   0.8,
			SeenCount:      2,
			CreatedAt:      ago(100),
		})
		insertLearning(t, db, Learning{
			ID:             "c1-recent",
			Type:           TypeInsight,
			Content:        "never injected but recent",
			Domain:         "dev",
			InjectionCount: 0,
			UsageCount:     0,
			QualityScore:   0.8,
			SeenCount:      1,
			CreatedAt:      ago(10),
		})
		insertLearning(t, db, Learning{
			ID:             "c1-has-injection",
			Type:           TypeInsight,
			Content:        "old but was injected",
			Domain:         "dev",
			InjectionCount: 1,
			UsageCount:     0,
			QualityScore:   0.8,
			SeenCount:      3, // seen multiple times: avoids criterion 4 trigger
			CreatedAt:      ago(100),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight: %v", err)
		}

		ids := archiveCandidateIDs(candidates)
		if !ids["c1-match"] {
			t.Error("c1-match (old, never injected, never used) should be archived")
		}
		if ids["c1-recent"] {
			t.Error("c1-recent should not be archived (too new)")
		}
		if ids["c1-has-injection"] {
			t.Error("c1-has-injection should not be archived (was injected)")
		}
	})

	t.Run("criterion 2: chronic non-compliance (injection>=5, rate<0.10)", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:             "c2-match",
			Type:           TypePattern,
			Content:        "chronic non-compliant learning",
			Domain:         "dev",
			InjectionCount: 5,
			ComplianceRate: 0.08,
			QualityScore:   0.6,
			SeenCount:      5,
			CreatedAt:      ago(10), // age doesn't matter for criterion 2
		})
		insertLearning(t, db, Learning{
			ID:             "c2-low-injection",
			Type:           TypePattern,
			Content:        "low injection count",
			Domain:         "dev",
			InjectionCount: 4, // below threshold
			ComplianceRate: 0.08,
			QualityScore:   0.6,
			SeenCount:      4,
			CreatedAt:      ago(10),
		})
		insertLearning(t, db, Learning{
			ID:             "c2-ok-rate",
			Type:           TypePattern,
			Content:        "high injection but ok compliance",
			Domain:         "dev",
			InjectionCount: 5,
			ComplianceRate: 0.10, // exactly at threshold — should NOT be archived
			QualityScore:   0.6,
			SeenCount:      5,
			CreatedAt:      ago(10),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight: %v", err)
		}

		ids := archiveCandidateIDs(candidates)
		if !ids["c2-match"] {
			t.Error("c2-match (injection>=5, rate=0.08) should be archived")
		}
		if ids["c2-low-injection"] {
			t.Error("c2-low-injection (injection=4) should not be archived")
		}
		if ids["c2-ok-rate"] {
			t.Error("c2-ok-rate (compliance_rate=0.10, not < 0.10) should not be archived")
		}
	})

	t.Run("criterion 3: low quality, never used, older than 60 days", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:           "c3-match",
			Type:         TypeError,
			Content:      "low quality old unused learning",
			Domain:       "dev",
			QualityScore: 0.15,
			UsageCount:   0,
			SeenCount:    1,
			CreatedAt:    ago(70),
		})
		insertLearning(t, db, Learning{
			ID:           "c3-recent",
			Type:         TypeError,
			Content:      "low quality but recent",
			Domain:       "dev",
			QualityScore: 0.15,
			UsageCount:   0,
			SeenCount:    1,
			CreatedAt:    ago(30), // not old enough
		})
		insertLearning(t, db, Learning{
			ID:           "c3-used",
			Type:         TypeError,
			Content:      "low quality old but was used",
			Domain:       "dev",
			QualityScore: 0.15,
			UsageCount:   1, // was used
			SeenCount:    3, // seen multiple times: avoids criterion 4 trigger
			CreatedAt:    ago(70),
		})
		insertLearning(t, db, Learning{
			ID:           "c3-boundary",
			Type:         TypeError,
			Content:      "exactly 0.20 quality old unused",
			Domain:       "dev",
			QualityScore: 0.20, // boundary: criterion requires < 0.2, so 0.20 should NOT match
			UsageCount:   0,
			SeenCount:    3, // seen multiple times: avoids criterion 4 trigger
			CreatedAt:    ago(70),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight: %v", err)
		}

		ids := archiveCandidateIDs(candidates)
		if !ids["c3-match"] {
			t.Error("c3-match (quality=0.15, used=0, old) should be archived")
		}
		if ids["c3-recent"] {
			t.Error("c3-recent should not be archived (too new)")
		}
		if ids["c3-used"] {
			t.Error("c3-used should not be archived (was used)")
		}
		if ids["c3-boundary"] {
			t.Error("c3-boundary (quality=0.20) should not be archived (criterion requires < 0.2)")
		}
	})

	t.Run("criterion 4: single observation, no embedding, older than 30 days", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:        "c4-match",
			Type:      TypeInsight,
			Content:   "single noisy observation",
			Domain:    "dev",
			SeenCount: 1,
			// Embedding: nil (no embedding)
			QualityScore: 0.5,
			CreatedAt:    ago(40),
		})
		insertLearning(t, db, Learning{
			ID:        "c4-recent",
			Type:      TypeInsight,
			Content:   "single observation but recent",
			Domain:    "dev",
			SeenCount: 1,
			CreatedAt: ago(10), // not old enough
		})
		insertLearning(t, db, Learning{
			ID:        "c4-multi-seen",
			Type:      TypeInsight,
			Content:   "seen more than once",
			Domain:    "dev",
			SeenCount: 2, // seen more than once
			CreatedAt: ago(40),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight: %v", err)
		}

		ids := archiveCandidateIDs(candidates)
		if !ids["c4-match"] {
			t.Error("c4-match (seen=1, no embedding, old) should be archived")
		}
		if ids["c4-recent"] {
			t.Error("c4-recent should not be archived (too new)")
		}
		if ids["c4-multi-seen"] {
			t.Error("c4-multi-seen (seen=2) should not be archived")
		}
	})

	t.Run("dry-run does not modify DB", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:             "dryrun-target",
			Type:           TypeInsight,
			Content:        "never injected old dry run target",
			Domain:         "dev",
			InjectionCount: 0,
			UsageCount:     0,
			QualityScore:   0.8,
			SeenCount:      1,
			CreatedAt:      ago(100),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight (dry-run): %v", err)
		}
		if len(candidates) == 0 {
			t.Fatal("expected at least one candidate in dry-run, got none")
		}

		// DB must be unchanged: archived flag still 0
		if got := getArchivedFlag(t, db, "dryrun-target"); got != 0 {
			t.Errorf("dry-run: archived flag = %d; want 0 (DB must not be modified)", got)
		}
	})

	t.Run("non-dry-run archives rows in DB", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:             "real-archive-target",
			Type:           TypeInsight,
			Content:        "never injected old real target",
			Domain:         "dev",
			InjectionCount: 0,
			UsageCount:     0,
			QualityScore:   0.8,
			SeenCount:      1,
			CreatedAt:      ago(100),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight: %v", err)
		}
		if len(candidates) == 0 {
			t.Fatal("expected at least one candidate, got none")
		}

		// DB must be updated: archived flag is 1
		if got := getArchivedFlag(t, db, "real-archive-target"); got != 1 {
			t.Errorf("non-dry-run: archived flag = %d; want 1", got)
		}
	})

	t.Run("archived learnings excluded from hybridSearch", func(t *testing.T) {
		db := newTestDB(t)
		const domain = "dev"
		const word = "zorglub" // distinctive term for FTS

		// Active learning — should appear in search results
		insertLearning(t, db, Learning{
			ID:           "active-search",
			Type:         TypePattern,
			Content:      word + " active learning content",
			Domain:       domain,
			SeenCount:    3,
			QualityScore: 0.9,
		})
		// Archived learning — same content/query match but must be excluded
		insertLearning(t, db, Learning{
			ID:           "archived-search",
			Type:         TypePattern,
			Content:      word + " archived learning content",
			Domain:       domain,
			SeenCount:    3,
			QualityScore: 0.9,
		})
		setArchived(t, db, "archived-search", 1)

		results, err := db.hybridSearch(domain, word, nil, 10, nil)
		if err != nil {
			t.Fatalf("hybridSearch: %v", err)
		}

		ids := make(map[string]bool, len(results))
		for _, l := range results {
			ids[l.ID] = true
		}

		if !ids["active-search"] {
			t.Error("active-search should appear in hybridSearch results")
		}
		if ids["archived-search"] {
			t.Error("archived-search (archived=1) must not appear in hybridSearch results")
		}
	})

	t.Run("StaleInjectedQuadrant: injection_count>100 and compliance_rate<0.25 archived", func(t *testing.T) {
		db := newTestDB(t)
		// Match: injection_count > 100 AND compliance_rate < 0.25
		insertLearning(t, db, Learning{
			ID:             "si-match",
			Type:           TypeInsight,
			Content:        "stale injected match",
			Domain:         "dev",
			InjectionCount: 150,
			ComplianceRate: 0.10,
			QualityScore:   0.8,
			SeenCount:      5,
			CreatedAt:      time.Now(),
		})
		// injection_count > 100 but compliance_rate >= 0.25 — should NOT match
		insertLearning(t, db, Learning{
			ID:             "si-good-rate",
			Type:           TypeInsight,
			Content:        "high injection but good compliance",
			Domain:         "dev",
			InjectionCount: 150,
			ComplianceRate: 0.25,
			QualityScore:   0.8,
			SeenCount:      5,
			CreatedAt:      time.Now(),
		})
		// injection_count <= 100 — should NOT match stale_injected (boundary: > 100 is false at 100)
		// compliance_rate set >= 0.10 to avoid criterion 2 (injection>=5, rate<0.10)
		insertLearning(t, db, Learning{
			ID:             "si-low-count",
			Type:           TypeInsight,
			Content:        "low injection count",
			Domain:         "dev",
			InjectionCount: 100,
			ComplianceRate: 0.15,
			QualityScore:   0.8,
			SeenCount:      5,
			CreatedAt:      time.Now(),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight: %v", err)
		}
		ids := archiveCandidateIDs(candidates)
		if !ids["si-match"] {
			t.Error("si-match (injection=150, rate=0.10) should be a stale_injected candidate")
		}
		if ids["si-good-rate"] {
			t.Error("si-good-rate (compliance_rate=0.25) should not be archived")
		}
		if ids["si-low-count"] {
			t.Error("si-low-count (injection_count=100) should not be archived")
		}
		// Verify the reason is "stale_injected"
		for _, c := range candidates {
			if c.ID == "si-match" && c.Reason != "stale_injected" {
				t.Errorf("si-match reason = %q; want stale_injected", c.Reason)
			}
		}
	})

	t.Run("DryRunNoMutation: stale_injected candidate not written when DryRun=true", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:             "si-dryrun",
			Type:           TypeInsight,
			Content:        "stale injected dry run target",
			Domain:         "dev",
			InjectionCount: 200,
			ComplianceRate: 0.05,
			QualityScore:   0.8,
			SeenCount:      5,
			CreatedAt:      time.Now(),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight dry-run: %v", err)
		}
		ids := archiveCandidateIDs(candidates)
		if !ids["si-dryrun"] {
			t.Fatal("si-dryrun should be returned as candidate in dry-run")
		}
		if got := getArchivedFlag(t, db, "si-dryrun"); got != 0 {
			t.Errorf("dry-run must not mutate DB: archived = %d; want 0", got)
		}
	})

	t.Run("domain filter restricts archival to specified domain", func(t *testing.T) {
		db := newTestDB(t)
		insertLearning(t, db, Learning{
			ID:             "domain-dev",
			Type:           TypeInsight,
			Content:        "never injected old dev learning",
			Domain:         "dev",
			InjectionCount: 0,
			UsageCount:     0,
			QualityScore:   0.8,
			SeenCount:      1,
			CreatedAt:      ago(100),
		})
		insertLearning(t, db, Learning{
			ID:             "domain-personal",
			Type:           TypeInsight,
			Content:        "never injected old personal learning",
			Domain:         "personal",
			InjectionCount: 0,
			UsageCount:     0,
			QualityScore:   0.8,
			SeenCount:      1,
			CreatedAt:      ago(100),
		})

		candidates, err := db.ArchiveDeadWeight(ctx, ArchiveOptions{DryRun: true, Domain: "dev"})
		if err != nil {
			t.Fatalf("ArchiveDeadWeight: %v", err)
		}

		ids := archiveCandidateIDs(candidates)
		if !ids["domain-dev"] {
			t.Error("domain-dev should be in candidates when Domain='dev'")
		}
		if ids["domain-personal"] {
			t.Error("domain-personal must not be in candidates when Domain='dev'")
		}
	})
}

// ---------------------------------------------------------------------------
// TestSchemaVersion
// ---------------------------------------------------------------------------

func TestSchemaVersionCreated(t *testing.T) {
	db := newTestDB(t)
	var version int
	if err := db.db.QueryRow(`SELECT version FROM schema_version`).Scan(&version); err != nil {
		t.Fatalf("querying schema_version: %v", err)
	}
	if version != 1 {
		t.Errorf("schema_version = %d; want 1", version)
	}
}

func TestSchemaVersionTooHigh(t *testing.T) {
	rawDB, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatalf("open in-memory db: %v", err)
	}
	defer rawDB.Close()

	// Manually create and seed schema_version with a future version.
	if _, err := rawDB.Exec(`CREATE TABLE schema_version (version INTEGER NOT NULL)`); err != nil {
		t.Fatalf("create schema_version: %v", err)
	}
	if _, err := rawDB.Exec(`INSERT INTO schema_version (version) VALUES (?)`, maxSupportedVersion+1); err != nil {
		t.Fatalf("seed schema_version: %v", err)
	}

	ldb := &DB{db: rawDB}
	if err := ldb.initSchema(); err == nil {
		t.Fatal("initSchema: expected error for version > maxSupportedVersion, got nil")
	}
}

// archiveCandidateIDs converts a candidate slice to an ID set for easy lookup.
func archiveCandidateIDs(candidates []ArchiveCandidate) map[string]bool {
	ids := make(map[string]bool, len(candidates))
	for _, c := range candidates {
		ids[c.ID] = true
	}
	return ids
}

// ---------------------------------------------------------------------------
// TestComplianceGate
// ---------------------------------------------------------------------------

func TestComplianceGate(t *testing.T) {
	// The compliance gate in hybridSearch SQL:
	//   AND (injection_count < 20 OR compliance_rate >= 0.15)
	//
	// Learnings with injection_count >= 20 AND compliance_rate < 0.15 are excluded.
	// The test uses FTS to drive scoring (no embeddings needed).
	const domain = "dev"
	const queryTerm = "xylophone" // distinctive term guaranteed to match only our test rows

	tests := []struct {
		name           string
		injections     int
		complianceRate float64
		wantIncluded   bool
		description    string
	}{
		{
			name:           "below injection threshold: always included",
			injections:     2,
			complianceRate: 0.05,
			wantIncluded:   true,
			description:    "injection_count=2 < 20, gate does not apply regardless of rate",
		},
		{
			name:           "exactly 20 injections, rate below 0.15: excluded",
			injections:     20,
			complianceRate: 0.10,
			wantIncluded:   false,
			description:    "injection_count=20 and rate=0.10 < 0.15 → excluded",
		},
		{
			name:           "exactly 20 injections, rate exactly 0.15: included",
			injections:     20,
			complianceRate: 0.15,
			wantIncluded:   true,
			description:    "injection_count=20 but rate=0.15 >= 0.15 → included",
		},
		{
			name:           "high injections, rate above threshold: included",
			injections:     25,
			complianceRate: 0.50,
			wantIncluded:   true,
			description:    "injection_count=25 but rate=0.50 >= 0.15 → included",
		},
		{
			name:           "high injections, rate below threshold: excluded",
			injections:     25,
			complianceRate: 0.05,
			wantIncluded:   false,
			description:    "injection_count=25 and rate=0.05 < 0.15 → excluded",
		},
		{
			name:           "zero injections: always included",
			injections:     0,
			complianceRate: 0.0,
			wantIncluded:   true,
			description:    "injection_count=0 < 20, gate does not apply",
		},
	}

	for i, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			db := newTestDB(t)
			id := "gate-" + string(rune('a'+i))
			insertLearning(t, db, Learning{
				ID:             id,
				Type:           TypePattern,
				Content:        queryTerm + " compliance gate test case " + tt.name,
				Domain:         domain,
				SeenCount:      3,
				QualityScore:   0.8,
				InjectionCount: tt.injections,
				ComplianceRate: tt.complianceRate,
			})

			results, err := db.hybridSearch(domain, queryTerm, nil, 10, nil)
			if err != nil {
				t.Fatalf("hybridSearch: %v", err)
			}

			found := false
			for _, l := range results {
				if l.ID == id {
					found = true
					break
				}
			}

			if tt.wantIncluded && !found {
				t.Errorf("%s: learning %s should be included in results (%s)", tt.name, id, tt.description)
			}
			if !tt.wantIncluded && found {
				t.Errorf("%s: learning %s should be excluded from results (%s)", tt.name, id, tt.description)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// TestScanLearningWithCompliance
// ---------------------------------------------------------------------------

func TestScanLearningWithCompliance(t *testing.T) {
	tests := []struct {
		name           string
		injectionCount int
		complianceRate float64
	}{
		{"zero injection, zero rate", 0, 0.0},
		{"non-zero injection, non-zero rate", 7, 0.42},
		{"high injection, high rate", 20, 0.95},
		{"high injection, low rate", 15, 0.06},
		{"exactly threshold boundary", 3, 0.15},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			db := newTestDB(t)
			id := "scan-" + tt.name

			insertLearning(t, db, Learning{
				ID:             id,
				Type:           TypePattern,
				Content:        "scan compliance test content",
				Domain:         "dev",
				SeenCount:      3,
				QualityScore:   0.75,
				InjectionCount: tt.injectionCount,
				ComplianceRate: tt.complianceRate,
			})

			// Use the same projection as hybridSearch to drive scanLearning.
			rows, err := db.db.Query(`
				SELECT id, type, content, context, domain, worker_name, workspace_id,
					tags, seen_count, used_count, quality_score, created_at, last_used_at, embedding,
					injection_count, compliance_rate
				FROM learnings WHERE id = ?
			`, id)
			if err != nil {
				t.Fatalf("query: %v", err)
			}
			defer rows.Close()

			if !rows.Next() {
				t.Fatal("no rows returned for inserted learning")
			}

			got, err := scanLearning(rows)
			if err != nil {
				t.Fatalf("scanLearning: %v", err)
			}

			if got.InjectionCount != tt.injectionCount {
				t.Errorf("InjectionCount = %d; want %d", got.InjectionCount, tt.injectionCount)
			}
			if got.ComplianceRate != tt.complianceRate {
				t.Errorf("ComplianceRate = %f; want %f", got.ComplianceRate, tt.complianceRate)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// TestComplianceWeightedDecay
// ---------------------------------------------------------------------------

func TestComplianceWeightedDecay(t *testing.T) {
	// decayScores() only applies to learnings older than 30 days and with
	// quality_score > 0.05. Decay rates:
	//   injection_count = 0                   → ×0.95 (5% decay)
	//   injection_count > 0, rate < 0.3       → ×0.85 (15% decay)
	//   injection_count > 0, 0.3 ≤ rate < 0.7 → ×0.95 (5% decay)
	//   injection_count > 0, rate ≥ 0.7       → ×0.98 (2% decay)
	const epsilon = 0.001
	const initialScore = 1.0
	oldDate := time.Now().Add(-35 * 24 * time.Hour)

	tests := []struct {
		name           string
		injectionCount int
		complianceRate float64
		createdAt      time.Time
		wantMultiplier float64
		wantDecayed    bool // true = expect score change; false = score unchanged
	}{
		{
			name:           "never injected (injection_count=0): 5% decay",
			injectionCount: 0,
			complianceRate: 0.0,
			createdAt:      oldDate,
			wantMultiplier: 0.95,
			wantDecayed:    true,
		},
		{
			name:           "low compliance (rate<0.3): 15% decay",
			injectionCount: 3,
			complianceRate: 0.20,
			createdAt:      oldDate,
			wantMultiplier: 0.85,
			wantDecayed:    true,
		},
		{
			name:           "mid compliance (0.3<=rate<0.7): 5% decay",
			injectionCount: 3,
			complianceRate: 0.50,
			createdAt:      oldDate,
			wantMultiplier: 0.95,
			wantDecayed:    true,
		},
		{
			name:           "high compliance (rate>=0.7): 2% decay",
			injectionCount: 3,
			complianceRate: 0.80,
			createdAt:      oldDate,
			wantMultiplier: 0.98,
			wantDecayed:    true,
		},
		{
			name:           "recent learning: no decay applied",
			injectionCount: 0,
			complianceRate: 0.0,
			createdAt:      time.Now(), // not old enough
			wantMultiplier: 1.0,
			wantDecayed:    false,
		},
		{
			name:           "low compliance boundary at exactly 0.3: mid rate applies",
			injectionCount: 3,
			complianceRate: 0.30, // >= 0.3 → mid, not low
			createdAt:      oldDate,
			wantMultiplier: 0.95,
			wantDecayed:    true,
		},
		{
			name:           "high compliance boundary at exactly 0.7: high rate applies",
			injectionCount: 3,
			complianceRate: 0.70, // >= 0.7 → high
			createdAt:      oldDate,
			wantMultiplier: 0.98,
			wantDecayed:    true,
		},
	}

	for i, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			db := newTestDB(t)
			id := "decay-" + string(rune('a'+i))

			insertLearning(t, db, Learning{
				ID:             id,
				Type:           TypeInsight,
				Content:        "decay test learning",
				Domain:         "dev",
				SeenCount:      1,
				QualityScore:   initialScore,
				InjectionCount: tt.injectionCount,
				ComplianceRate: tt.complianceRate,
				CreatedAt:      tt.createdAt,
			})

			db.decayScores()

			got := getQualityScore(t, db, id)
			want := initialScore * tt.wantMultiplier

			if tt.wantDecayed {
				if abs64(got-want) > epsilon {
					t.Errorf("quality_score = %.4f; want %.4f (multiplier %.2f) for %s",
						got, want, tt.wantMultiplier, tt.name)
				}
			} else {
				if abs64(got-initialScore) > epsilon {
					t.Errorf("quality_score = %.4f; want %.4f (no decay expected) for %s",
						got, initialScore, tt.name)
				}
			}
		})
	}
}

func abs64(x float64) float64 {
	if x < 0 {
		return -x
	}
	return x
}

// ---------------------------------------------------------------------------
// TestUnionSearchStrategy
// ---------------------------------------------------------------------------

// TestUnionSearchStrategy verifies that hybridSearch uses a union of FTS and
// embedding candidates so that entries with strong lexical match but weak
// semantic similarity AND entries with weak lexical match but strong semantic
// similarity both appear in results.
//
// Under the old FTS-first narrowing, the semantically-similar-but-lexically-
// different entry was silently excluded because the code only scored FTS hits.
// The union strategy fixes that.
func TestUnionSearchStrategy(t *testing.T) {
	db := newTestDB(t)

	// queryEmb is a unit vector along dimension 0.
	queryEmb := []float32{1, 0, 0}

	// lexicalStrong: content contains the query keyword "zorglub" (strong FTS
	// match), but its embedding is orthogonal to queryEmb (cosine = 0, so it
	// would NOT appear in embedding top-50 alone).
	lexEmb := []float32{0, 1, 0} // orthogonal → cosine with queryEmb = 0
	insertLearning(t, db, Learning{
		ID:           "lexical-strong",
		Type:         TypePattern,
		Content:      "zorglub frembgorp xylophone query keyword present",
		Domain:       "dev",
		SeenCount:    3,
		QualityScore: 0.7,
		Embedding:    lexEmb,
	})

	// semanticStrong: content has NO overlap with the query (weak FTS — it
	// will not appear in FTS results for "zorglub"), but its embedding is
	// nearly parallel to queryEmb (cosine ≈ 1, strong semantic match).
	semEmb := []float32{1, 0, 0} // same direction → cosine with queryEmb = 1
	insertLearning(t, db, Learning{
		ID:           "semantic-strong",
		Type:         TypePattern,
		Content:      "qwerty asdf zxcv completely different vocabulary",
		Domain:       "dev",
		SeenCount:    3,
		QualityScore: 0.7,
		Embedding:    semEmb,
	})

	results, err := db.hybridSearch("dev", "zorglub", queryEmb, 10, nil)
	if err != nil {
		t.Fatalf("hybridSearch: %v", err)
	}

	ids := make(map[string]bool, len(results))
	for _, l := range results {
		ids[l.ID] = true
	}

	if !ids["lexical-strong"] {
		t.Error("lexical-strong should appear: it has a strong FTS match for 'zorglub'")
	}
	if !ids["semantic-strong"] {
		t.Error("semantic-strong should appear: it has strong embedding cosine similarity to query vector")
	}
}

// ---------------------------------------------------------------------------
// TestHybridScoringOrdering
// ---------------------------------------------------------------------------

// TestHybridScoringOrdering verifies that the hybrid relevance formula ranks a
// perfect semantic match above a barely-relevant but recent, high-quality entry.
//
// Under the old formula (quality * 0.6 + recency * 0.4), high quality + recency
// always outranked a good semantic match. The new formula:
//
//	final = relevance * 0.5 + quality * 0.3 + recency * 0.2
//
// ensures a perfect cosine match (relevance = 1.0) wins even against an entry
// with quality = 1.0 and recency = 1.0 but near-zero cosine similarity (~0.1).
//
// Expected scores:
//
//	perfect-semantic:     1.0*0.5 + 0.5*0.3 + 0.4*0.2 = 0.73
//	barely-relevant:     ~0.1*0.5 + 1.0*0.3 + 1.0*0.2 = 0.55
func TestHybridScoringOrdering(t *testing.T) {
	db := newTestDB(t)

	queryEmb := []float32{1, 0, 0}

	// perfectSemantic: embedding exactly matches query direction (cosine = 1.0).
	// Low quality and old — would have ranked poorly under the old formula.
	insertLearning(t, db, Learning{
		ID:           "perfect-semantic",
		Type:         TypePattern,
		Content:      "qwerty asdf zxcv completely different vocabulary",
		Domain:       "dev",
		SeenCount:    3,
		QualityScore: 0.5,
		Embedding:    []float32{1, 0, 0},                    // cosine with queryEmb = 1.0
		CreatedAt:    time.Now().Add(-200 * 24 * time.Hour), // older than 180d → recency 0.4
	})

	// barelyRelevant: embedding nearly orthogonal to query (cosine ≈ 0.1).
	// Perfect quality, brand new — would have ranked first under old formula.
	insertLearning(t, db, Learning{
		ID:           "barely-relevant",
		Type:         TypePattern,
		Content:      "blorp fremble wubzap zorkle unrelated words too",
		Domain:       "dev",
		SeenCount:    3,
		QualityScore: 1.0,
		Embedding:    []float32{0.1, 0.995, 0}, // cosine with queryEmb ≈ 0.1
		CreatedAt:    time.Now(),               // brand new → recency 1.0
	})

	// Drive scoring via embedding only — no FTS query term so ordering depends
	// entirely on the relevance formula, not text match.
	results, err := db.hybridSearch("dev", "", queryEmb, 10, nil)
	if err != nil {
		t.Fatalf("hybridSearch: %v", err)
	}
	if len(results) < 2 {
		t.Fatalf("expected at least 2 results, got %d", len(results))
	}
	if results[0].ID != "perfect-semantic" {
		t.Errorf("results[0] = %q; want %q — perfect semantic match must rank first", results[0].ID, "perfect-semantic")
	}
	if results[1].ID != "barely-relevant" {
		t.Errorf("results[1] = %q; want %q", results[1].ID, "barely-relevant")
	}
}

// ---------------------------------------------------------------------------
// TestEmbeddingScanBeyond500
// ---------------------------------------------------------------------------

// TestEmbeddingScanBeyond500 verifies that hybridSearch scans ALL embeddings in
// the domain, not just the first 500. The most semantically relevant learning is
// inserted at position 551 (beyond the old LIMIT 500 cutoff) and must appear in
// top results.
func TestEmbeddingScanBeyond500(t *testing.T) {
	db := newTestDB(t)
	const domain = "scan-beyond"
	const n = 600

	// queryEmb is a unit vector along dimension 0.
	queryEmb := []float32{1, 0, 0}

	// Filler entries: orthogonal to queryEmb (cosine = 0) — will not rank.
	fillerEmb := []float32{0, 1, 0}

	// targetEmb: near-perfect match for queryEmb (cosine ≈ 1.0).
	// Inserted at position 551 — beyond the old LIMIT 500 window.
	targetEmb := []float32{0.9999, 0.01, 0}
	const targetPos = 551

	for i := 0; i < n; i++ {
		id := fmt.Sprintf("learning-%04d", i)
		emb := fillerEmb
		content := fmt.Sprintf("filler content learning number %d", i)
		if i == targetPos {
			id = "target-551"
			emb = targetEmb
			content = "target semantically relevant learning inserted beyond position 500"
		}
		insertLearning(t, db, Learning{
			ID:           id,
			Type:         TypePattern,
			Content:      content,
			Domain:       domain,
			SeenCount:    1,
			QualityScore: 0.5,
			Embedding:    emb,
		})
	}

	results, err := db.hybridSearch(domain, "", queryEmb, 10, nil)
	if err != nil {
		t.Fatalf("hybridSearch: %v", err)
	}

	found := false
	for _, l := range results {
		if l.ID == "target-551" {
			found = true
			break
		}
	}
	if !found {
		ids := make([]string, 0, len(results))
		for _, l := range results {
			ids = append(ids, l.ID)
		}
		t.Errorf("target-551 (inserted at position %d) not found in top results; got: %v", targetPos, ids)
	}
}

// ---------------------------------------------------------------------------
// FindTopByQuality (cold-start selection)
// ---------------------------------------------------------------------------

func TestFindTopByQuality_EmptyDB(t *testing.T) {
	db := newTestDB(t)
	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) != 0 {
		t.Errorf("expected 0 results for empty DB, got %d", len(results))
	}
}

func TestFindTopByQuality_RanksByQualityTimesRecency(t *testing.T) {
	db := newTestDB(t)

	// Distinct types so the per-type cap (maxPerType=2) does not drop rows —
	// this test targets relevance ordering, not the diversity filter.

	// high quality but old — should rank below high-quality recent
	insertLearning(t, db, Learning{
		ID:           "old-high",
		Type:         TypePattern,
		Content:      "Old but high quality learning.",
		Domain:       "dev",
		QualityScore: 0.9,
		CreatedAt:    time.Now().Add(-200 * 24 * time.Hour), // >180 days → recency 0.4
	})
	// moderate quality, recent — should rank below old-high (0.7*1.0=0.7 vs 0.9*0.4=0.36)
	insertLearning(t, db, Learning{
		ID:           "recent-mid",
		Type:         TypeDecision,
		Content:      "Recent but mid quality learning.",
		Domain:       "dev",
		QualityScore: 0.7,
		CreatedAt:    time.Now().Add(-5 * 24 * time.Hour), // <30 days → recency 1.0
	})
	// best: high quality and recent
	insertLearning(t, db, Learning{
		ID:           "recent-high",
		Type:         TypeInsight,
		Content:      "Recent and high quality learning.",
		Domain:       "dev",
		QualityScore: 0.9,
		CreatedAt:    time.Now().Add(-3 * 24 * time.Hour), // <30 days → recency 1.0
	})

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) != 3 {
		t.Fatalf("expected 3 results, got %d", len(results))
	}
	// recent-high (0.9*1.0=0.90) > recent-mid (0.7*1.0=0.70) > old-high (0.9*0.4=0.36)
	if results[0].ID != "recent-high" {
		t.Errorf("first result should be recent-high, got %s", results[0].ID)
	}
	if results[1].ID != "recent-mid" {
		t.Errorf("second result should be recent-mid, got %s", results[1].ID)
	}
	if results[2].ID != "old-high" {
		t.Errorf("third result should be old-high, got %s", results[2].ID)
	}
}

func TestFindTopByQuality_RespectsDomain(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "dev-learning",
		Type:         TypeInsight,
		Content:      "Dev domain learning.",
		Domain:       "dev",
		QualityScore: 0.8,
		CreatedAt:    time.Now(),
	})
	insertLearning(t, db, Learning{
		ID:           "work-learning",
		Type:         TypeInsight,
		Content:      "Work domain learning.",
		Domain:       "work",
		QualityScore: 0.9,
		CreatedAt:    time.Now(),
	})

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) != 1 {
		t.Fatalf("expected 1 result for 'dev' domain, got %d", len(results))
	}
	if results[0].ID != "dev-learning" {
		t.Errorf("expected dev-learning, got %s", results[0].ID)
	}
}

func TestFindTopByQuality_RespectsLimit(t *testing.T) {
	db := newTestDB(t)
	// Cycle across the 5 learning types so the per-type cap (2) does not
	// shrink the candidate pool below the requested limit.
	types := []LearningType{TypeInsight, TypePattern, TypeError, TypeSource, TypeDecision}
	for i := 0; i < 10; i++ {
		insertLearning(t, db, Learning{
			ID:           fmt.Sprintf("learn-%02d", i),
			Type:         types[i%len(types)],
			Content:      fmt.Sprintf("Learning number %d.", i),
			Domain:       "dev",
			QualityScore: 0.8,
			CreatedAt:    time.Now(),
		})
	}

	results, err := db.FindTopByQuality("dev", 5)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) != 5 {
		t.Errorf("expected 5 results with limit=5, got %d", len(results))
	}
}

func TestFindTopByQuality_ZeroLimitDefaultsTwenty(t *testing.T) {
	db := newTestDB(t)
	for i := 0; i < 25; i++ {
		insertLearning(t, db, Learning{
			ID:           fmt.Sprintf("zl-%02d", i),
			Type:         TypeInsight,
			Content:      fmt.Sprintf("Zero limit learning %d.", i),
			Domain:       "dev",
			QualityScore: 0.8,
			CreatedAt:    time.Now(),
		})
	}

	results, err := db.FindTopByQuality("dev", 0)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) > 20 {
		t.Errorf("expected at most 20 results with limit=0 (defaults to 20), got %d", len(results))
	}
}

func TestFindTopByQuality_SkipsArchived(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "active",
		Type:         TypeInsight,
		Content:      "Active learning.",
		Domain:       "dev",
		QualityScore: 0.8,
		CreatedAt:    time.Now(),
	})
	insertLearning(t, db, Learning{
		ID:           "archived",
		Type:         TypeInsight,
		Content:      "Archived learning.",
		Domain:       "dev",
		QualityScore: 0.9,
		CreatedAt:    time.Now(),
	})
	setArchived(t, db, "archived", 1)

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, l := range results {
		if l.ID == "archived" {
			t.Error("archived learning should not appear in FindTopByQuality results")
		}
	}
}

func TestFindTopByQuality_RespectsComplianceFilter(t *testing.T) {
	db := newTestDB(t)
	// Should appear: injection_count < 3
	insertLearning(t, db, Learning{
		ID:             "low-inject",
		Type:           TypeInsight,
		Content:        "Low injection count.",
		Domain:         "dev",
		QualityScore:   0.8,
		InjectionCount: 2,
		ComplianceRate: 0.05,
		CreatedAt:      time.Now(),
	})
	// Should appear: injection_count >= 3 but compliance_rate >= 0.15
	insertLearning(t, db, Learning{
		ID:             "high-inject-good-compliance",
		Type:           TypeInsight,
		Content:        "High injection, good compliance.",
		Domain:         "dev",
		QualityScore:   0.8,
		InjectionCount: 5,
		ComplianceRate: 0.20,
		CreatedAt:      time.Now(),
	})
	// Should NOT appear: injection_count >= 20 and compliance_rate < 0.15
	insertLearning(t, db, Learning{
		ID:             "overinjected",
		Type:           TypeInsight,
		Content:        "Over-injected with poor compliance.",
		Domain:         "dev",
		QualityScore:   0.9,
		InjectionCount: 20,
		ComplianceRate: 0.05,
		CreatedAt:      time.Now(),
	})

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	ids := make(map[string]bool, len(results))
	for _, l := range results {
		ids[l.ID] = true
	}
	if !ids["low-inject"] {
		t.Error("low-inject should appear (injection_count < 20)")
	}
	if !ids["high-inject-good-compliance"] {
		t.Error("high-inject-good-compliance should appear (compliance_rate >= 0.15)")
	}
	if ids["overinjected"] {
		t.Error("overinjected should be filtered out (injection_count >= 20 and compliance_rate < 0.15)")
	}
}

// ---------------------------------------------------------------------------
// FindTopByQuality — stale-injected filter (G5)
// ---------------------------------------------------------------------------

// TestFindTopByQuality_ExcludesStaleInjected verifies rows with injection_count > 100
// and compliance_rate < 0.25 are excluded from FindTopByQuality results.
func TestFindTopByQuality_ExcludesStaleInjected(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:             "stale-injected",
		Type:           TypeInsight,
		Content:        "Over-injected stale learning with low compliance.",
		Domain:         "dev",
		QualityScore:   0.9,
		InjectionCount: 101,
		ComplianceRate: 0.10,
		CreatedAt:      time.Now(),
	})

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, l := range results {
		if l.ID == "stale-injected" {
			t.Error("stale-injected (injection_count=101, compliance_rate=0.10) must be excluded")
		}
	}
}

// TestFindTopByQuality_IncludesHealthyRow verifies a row with high injection_count
// but compliance_rate >= 0.25 is NOT excluded by the stale-injected filter.
func TestFindTopByQuality_IncludesHealthyRow(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:             "healthy-high-inject",
		Type:           TypeInsight,
		Content:        "High injection count but healthy compliance.",
		Domain:         "dev",
		QualityScore:   0.8,
		InjectionCount: 150,
		ComplianceRate: 0.30,
		CreatedAt:      time.Now(),
	})

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	found := false
	for _, l := range results {
		if l.ID == "healthy-high-inject" {
			found = true
		}
	}
	if !found {
		t.Error("healthy-high-inject (injection_count=150, compliance_rate=0.30) should appear")
	}
}

// TestFindTopByQuality_IncludesLowInjectRow verifies a row with low injection_count
// regardless of compliance_rate is not caught by the stale-injected filter.
func TestFindTopByQuality_IncludesLowInjectRow(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:             "low-inject-any-compliance",
		Type:           TypeInsight,
		Content:        "Low injection count, compliance does not matter for this filter.",
		Domain:         "dev",
		QualityScore:   0.8,
		InjectionCount: 2,
		ComplianceRate: 0.05,
		CreatedAt:      time.Now(),
	})

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	found := false
	for _, l := range results {
		if l.ID == "low-inject-any-compliance" {
			found = true
		}
	}
	if !found {
		t.Error("low-inject-any-compliance (injection_count=2) should appear (not stale-injected)")
	}
}

// ---------------------------------------------------------------------------
// FindTopByQuality — per-type cap + MMR diversity (TRK-512)
// ---------------------------------------------------------------------------

// TestFindTopByQuality_PerTypeCap seeds many rows of three types and asserts
// that no type appears more than maxPerType (=2) times in the result.
func TestFindTopByQuality_PerTypeCap(t *testing.T) {
	db := newTestDB(t)
	now := time.Now()

	// 4 insight + 3 pattern + 2 decision; decreasing quality so SQL order
	// pulls them in mixed. With 2×limit=10 candidates fetched and cap=2 per
	// type, the pool collapses to at most 2+2+2 = 6 entries.
	spec := []struct {
		t     LearningType
		count int
	}{
		{TypeInsight, 4},
		{TypePattern, 3},
		{TypeDecision, 2},
	}
	q := 0.90
	for _, s := range spec {
		for i := 0; i < s.count; i++ {
			insertLearning(t, db, Learning{
				ID:           fmt.Sprintf("%s-%d", s.t, i),
				Type:         s.t,
				Content:      fmt.Sprintf("%s learning %d", s.t, i),
				Domain:       "dev",
				QualityScore: q,
				CreatedAt:    now,
			})
			q -= 0.01
		}
	}

	results, err := db.FindTopByQuality("dev", 5)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) > 5 {
		t.Errorf("expected ≤ 5 results (limit), got %d", len(results))
	}
	counts := make(map[LearningType]int)
	for _, l := range results {
		counts[l.Type]++
	}
	for typ, c := range counts {
		if c > maxPerType {
			t.Errorf("type %s appears %d times, exceeds per-type cap %d", typ, c, maxPerType)
		}
	}
}

// TestFindTopByQuality_MMRDiversityOverQuality verifies that MMR prefers a
// more topically diverse candidate over a higher-quality near-duplicate.
func TestFindTopByQuality_MMRDiversityOverQuality(t *testing.T) {
	db := newTestDB(t)
	now := time.Now()

	// 5 distinct types so per-type cap does not interfere.
	// Embeddings: A/B/E cluster around axis X; C is orthogonal on Y; D on Z.
	// Quality rank: A > B > C > D > E.
	seed := []struct {
		id  string
		typ LearningType
		q   float64
		emb []float32
	}{
		{"A-top", TypeInsight, 0.95, []float32{1, 0, 0}},
		{"B-dup", TypePattern, 0.90, []float32{1, 0, 0}},
		{"C-div", TypeDecision, 0.80, []float32{0, 1, 0}},
		{"D-alt", TypeSource, 0.70, []float32{0, 0, 1}},
		{"E-dup", TypeError, 0.60, []float32{1, 0, 0}},
	}
	for _, s := range seed {
		insertLearning(t, db, Learning{
			ID:           s.id,
			Type:         s.typ,
			Content:      s.id + " content",
			Domain:       "dev",
			QualityScore: s.q,
			CreatedAt:    now,
			Embedding:    s.emb,
		})
	}

	results, err := db.FindTopByQuality("dev", 2)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) != 2 {
		t.Fatalf("expected 2 results, got %d", len(results))
	}
	if results[0].ID != "A-top" {
		t.Errorf("first pick should be highest-quality A-top, got %s", results[0].ID)
	}
	// A raw quality-only ranker would pick B-dup second. MMR should choose
	// C-div (orthogonal embedding) despite its lower quality.
	if results[1].ID != "C-div" {
		t.Errorf("second pick should be diverse C-div (MMR), got %s", results[1].ID)
	}
}

// TestFindTopByQuality_NoEmbeddings_Fallback verifies the function tolerates
// rows without embeddings: MMR diversity cost falls back to 0 and relevance
// ordering determines the final slice.
func TestFindTopByQuality_NoEmbeddings_Fallback(t *testing.T) {
	db := newTestDB(t)
	now := time.Now()

	// 5 rows, 5 types, no embeddings, decreasing quality.
	seed := []struct {
		id  string
		typ LearningType
		q   float64
	}{
		{"n1", TypeInsight, 0.90},
		{"n2", TypePattern, 0.80},
		{"n3", TypeDecision, 0.70},
		{"n4", TypeSource, 0.60},
		{"n5", TypeError, 0.50},
	}
	for _, s := range seed {
		insertLearning(t, db, Learning{
			ID:           s.id,
			Type:         s.typ,
			Content:      s.id + " content",
			Domain:       "dev",
			QualityScore: s.q,
			CreatedAt:    now,
		})
	}

	results, err := db.FindTopByQuality("dev", 3)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) != 3 {
		t.Fatalf("expected 3 results, got %d", len(results))
	}
	// Without embeddings, diversity cost is 0 for every candidate → MMR
	// score reduces to lambda*relevance → pure quality order.
	wantOrder := []string{"n1", "n2", "n3"}
	for i, want := range wantOrder {
		if results[i].ID != want {
			t.Errorf("position %d: want %s, got %s", i, want, results[i].ID)
		}
	}
}

// TestFindTopByQuality_FewerCandidatesThanLimit verifies the function returns
// the available candidates without panicking when the pool is smaller than
// the requested limit.
func TestFindTopByQuality_FewerCandidatesThanLimit(t *testing.T) {
	db := newTestDB(t)
	now := time.Now()

	seed := []struct {
		id  string
		typ LearningType
	}{
		{"few-1", TypeInsight},
		{"few-2", TypePattern},
		{"few-3", TypeDecision},
	}
	for _, s := range seed {
		insertLearning(t, db, Learning{
			ID:           s.id,
			Type:         s.typ,
			Content:      s.id,
			Domain:       "dev",
			QualityScore: 0.8,
			CreatedAt:    now,
		})
	}

	results, err := db.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) != 3 {
		t.Errorf("expected 3 results (pool < limit), got %d", len(results))
	}
}

// TestFindTopByQuality_LimitZero regression-guards the default: limit=0 must
// expand to 20 internally (the legacy default) instead of returning nothing.
func TestFindTopByQuality_LimitZero(t *testing.T) {
	db := newTestDB(t)
	now := time.Now()

	// 22 rows across 11 distinct (type, sub-key) buckets so the per-type cap
	// does not shrink the pool below the default limit of 20 — we cycle
	// through 5 types (cap gives 2 per type = 10) plus we lean on MMR to
	// cap at 20 regardless.
	types := []LearningType{TypeInsight, TypePattern, TypeError, TypeSource, TypeDecision}
	for i := 0; i < 22; i++ {
		insertLearning(t, db, Learning{
			ID:           fmt.Sprintf("lz-%02d", i),
			Type:         types[i%len(types)],
			Content:      fmt.Sprintf("limit-zero learning %d", i),
			Domain:       "dev",
			QualityScore: 0.8,
			CreatedAt:    now,
		})
	}

	results, err := db.FindTopByQuality("dev", 0)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(results) == 0 {
		t.Fatal("expected non-empty results when limit=0 (should default to 20)")
	}
	if len(results) > 20 {
		t.Errorf("expected ≤20 results when limit=0 (default), got %d", len(results))
	}
}

// ---------------------------------------------------------------------------
// TestUpdateQualityScores
// ---------------------------------------------------------------------------

func TestUpdateQualityScores_TierMapping(t *testing.T) {
	db := newTestDB(t)
	ctx := context.Background()

	insertLearning(t, db, Learning{ID: "ins", Type: TypeInsight, Content: "x", Domain: "dev", SeenCount: 1})
	insertLearning(t, db, Learning{ID: "pat", Type: TypePattern, Content: "x", Domain: "dev", SeenCount: 1})
	insertLearning(t, db, Learning{ID: "src", Type: TypeSource, Content: "x", Domain: "dev", SeenCount: 1})

	n, err := db.UpdateQualityScores(ctx)
	if err != nil {
		t.Fatalf("UpdateQualityScores: %v", err)
	}
	if n != 3 {
		t.Errorf("rows affected: got %d, want 3", n)
	}

	const epsilon = 0.001
	cases := []struct {
		id   string
		want float64
	}{
		{"ins", 0.75},   // 1.0 * 1.0 * 1.0 * 0.75
		{"pat", 0.525},  // 0.7 * 1.0 * 1.0 * 0.75
		{"src", 0.30},   // 0.4 * 1.0 * 1.0 * 0.75
	}
	for _, c := range cases {
		got := getQualityScore(t, db, c.id)
		if got < c.want-epsilon || got > c.want+epsilon {
			t.Errorf("id=%s quality_score: got %.4f, want %.4f", c.id, got, c.want)
		}
	}
}

func TestUpdateQualityScores_InjectionBoost(t *testing.T) {
	db := newTestDB(t)
	ctx := context.Background()

	insertLearning(t, db, Learning{ID: "ins", Type: TypeInsight, Content: "x", Domain: "dev", SeenCount: 1})

	// Record 6 injections (bucket 6-10 → +0.4 boost)
	ids := make([]string, 6)
	for i := range ids {
		ids[i] = "ins"
	}
	if err := db.RecordInjections(ctx, ids); err != nil {
		t.Fatalf("RecordInjections: %v", err)
	}

	if _, err := db.UpdateQualityScores(ctx); err != nil {
		t.Fatalf("UpdateQualityScores: %v", err)
	}

	// 1.0 * 1.0 * (1+0.4) * 0.75 = 1.05
	const want = 1.05
	const epsilon = 0.001
	got := getQualityScore(t, db, "ins")
	if got < want-epsilon || got > want+epsilon {
		t.Errorf("quality_score: got %.4f, want %.4f", got, want)
	}
}

func TestUpdateQualityScores_ComplianceMultiplier(t *testing.T) {
	db := newTestDB(t)
	ctx := context.Background()

	insertLearning(t, db, Learning{ID: "ins", Type: TypeInsight, Content: "x", Domain: "dev", SeenCount: 1})

	// Record 10 injections (bucket >10 → +0.5; but we need ≤10 for +0.4 bucket; 10 is in bucket <=10)
	ids := make([]string, 10)
	for i := range ids {
		ids[i] = "ins"
	}
	if err := db.RecordInjections(ctx, ids); err != nil {
		t.Fatalf("RecordInjections: %v", err)
	}
	// Record 10 compliance-positive events (compliance_rate = 1.0)
	for i := 0; i < 10; i++ {
		if err := db.RecordCompliance(ctx, "ins", true); err != nil {
			t.Fatalf("RecordCompliance: %v", err)
		}
	}

	if _, err := db.UpdateQualityScores(ctx); err != nil {
		t.Fatalf("UpdateQualityScores: %v", err)
	}

	// 1.0 * 1.0 * (1+0.4) * (0.5 + 0.5*1.0) = 1.0 * 1.0 * 1.4 * 1.0 = 1.40
	const want = 1.40
	const epsilon = 0.001
	got := getQualityScore(t, db, "ins")
	if got < want-epsilon || got > want+epsilon {
		t.Errorf("quality_score: got %.4f, want %.4f", got, want)
	}
}

func TestUpdateQualityScores_ExcludesArchived(t *testing.T) {
	db := newTestDB(t)
	ctx := context.Background()

	insertLearning(t, db, Learning{ID: "active", Type: TypeInsight, Content: "x", Domain: "dev", SeenCount: 1, QualityScore: 0.5})
	insertLearning(t, db, Learning{ID: "archived", Type: TypeInsight, Content: "y", Domain: "dev", SeenCount: 1, QualityScore: 0.5})

	// Mark second row as archived via direct SQL (same pattern used in setArchived helper)
	if _, err := db.db.Exec("UPDATE learnings SET archived = 1 WHERE id = ?", "archived"); err != nil {
		t.Fatalf("mark archived: %v", err)
	}

	n, err := db.UpdateQualityScores(ctx)
	if err != nil {
		t.Fatalf("UpdateQualityScores: %v", err)
	}
	if n != 1 {
		t.Errorf("rows affected: got %d, want 1", n)
	}

	// Active row should be rescored (≠ 0.5)
	active := getQualityScore(t, db, "active")
	if active == 0.5 {
		t.Error("active row quality_score was not updated")
	}

	// Archived row must stay at 0.5
	archived := getQualityScore(t, db, "archived")
	const epsilon = 0.001
	if archived < 0.5-epsilon || archived > 0.5+epsilon {
		t.Errorf("archived row quality_score changed: got %.4f, want 0.5", archived)
	}
}

func TestUpdateQualityScores_EmptyDB(t *testing.T) {
	db := newTestDB(t)
	ctx := context.Background()

	n, err := db.UpdateQualityScores(ctx)
	if err != nil {
		t.Fatalf("UpdateQualityScores on empty DB: %v", err)
	}
	if n != 0 {
		t.Errorf("rows affected: got %d, want 0", n)
	}
}

// ---------------------------------------------------------------------------
// FindTopByQuality — threshold fix: highly-injected rows surface at limit=N
// ---------------------------------------------------------------------------

// TestFindTopByQuality_LimitRespectsHighlyInjectedRows verifies that rows with
// injection_count=10 (previously excluded by the old threshold of 3) are returned
// when limit=N and there are ≥N eligible candidates.
func TestFindTopByQuality_LimitRespectsHighlyInjectedRows(t *testing.T) {
	db := newTestDB(t)
	types := []LearningType{TypeInsight, TypePattern, TypeError, TypeSource, TypeDecision}
	const numCandidates = 10
	const limit = 10
	for i := 0; i < numCandidates; i++ {
		insertLearning(t, db, Learning{
			ID:             fmt.Sprintf("hi-inject-%02d", i),
			Type:           types[i%len(types)],
			Content:        fmt.Sprintf("Highly injected learning %d.", i),
			Domain:         "dev",
			QualityScore:   0.8,
			InjectionCount: 10, // would have been filtered with old threshold of 3
			ComplianceRate: 0.0,
			CreatedAt:      time.Now(),
		})
	}

	results, err := db.FindTopByQuality("dev", limit)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	want := min(limit, numCandidates)
	if len(results) != want {
		t.Errorf("FindTopByQuality(limit=%d) with %d candidates: got %d rows, want %d",
			limit, numCandidates, len(results), want)
	}
}

// TestFindTopByQuality_TypeSkewedPoolReturnsLimitedByLimit verifies that when
// the candidate pool is dominated by a single type, FindTopByQuality still
// returns exactly limit results (rather than being capped at the old
// maxPerType=2 ceiling). With maxPerType=5 and a pool of 10 TypeInsight rows,
// requesting limit=5 must yield 5 results.
func TestFindTopByQuality_TypeSkewedPoolReturnsLimitedByLimit(t *testing.T) {
	t.Run("single-type pool fills limit up to maxPerType", func(t *testing.T) {
		db := newTestDB(t)
		// Insert 10 rows of the same type. With maxPerType=5 the cap allows 5
		// through; limit=5 so we expect exactly 5.
		for i := 0; i < 10; i++ {
			insertLearning(t, db, Learning{
				ID:           fmt.Sprintf("skewed-%02d", i),
				Type:         TypeInsight,
				Content:      fmt.Sprintf("Skewed pool learning %d.", i),
				Domain:       "dev",
				QualityScore: 0.9 - float64(i)*0.01,
				CreatedAt:    time.Now(),
			})
		}

		results, err := db.FindTopByQuality("dev", 5)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if len(results) != 5 {
			t.Errorf("expected 5 results (limit=5, maxPerType=5), got %d", len(results))
		}
		for _, l := range results {
			if l.Type != TypeInsight {
				t.Errorf("unexpected type %s in results", l.Type)
			}
		}
	})

	t.Run("per-type cap still applies within limit", func(t *testing.T) {
		db := newTestDB(t)
		// 10 TypeInsight + 1 TypePattern. With limit=6, per-type cap of 5 means
		// at most 5 TypeInsight; the one TypePattern fills the 6th slot.
		for i := 0; i < 10; i++ {
			insertLearning(t, db, Learning{
				ID:           fmt.Sprintf("cap-insight-%02d", i),
				Type:         TypeInsight,
				Content:      fmt.Sprintf("Cap insight learning %d.", i),
				Domain:       "dev",
				QualityScore: 0.9 - float64(i)*0.01,
				CreatedAt:    time.Now(),
			})
		}
		insertLearning(t, db, Learning{
			ID:           "cap-pattern-0",
			Type:         TypePattern,
			Content:      "Cap pattern learning.",
			Domain:       "dev",
			QualityScore: 0.5,
			CreatedAt:    time.Now(),
		})

		results, err := db.FindTopByQuality("dev", 6)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if len(results) != 6 {
			t.Errorf("expected 6 results (limit=6), got %d", len(results))
		}
		counts := make(map[LearningType]int)
		for _, l := range results {
			counts[l.Type]++
		}
		if counts[TypeInsight] > maxPerType {
			t.Errorf("TypeInsight appears %d times, exceeds per-type cap %d", counts[TypeInsight], maxPerType)
		}
	})
}

// ---------------------------------------------------------------------------
// RecordInjections — last_used_at and injection_count write path
// ---------------------------------------------------------------------------

// TestRecordInjections_UpdatesLastUsedAtAndCount verifies that RecordInjections
// writes last_used_at = datetime('now') and increments injection_count by 1.
func TestRecordInjections_UpdatesLastUsedAtAndCount(t *testing.T) {
	db := newTestDB(t)
	ctx := context.Background()

	const initialCount = 5
	insertLearning(t, db, Learning{
		ID:             "inject-target",
		Type:           TypeInsight,
		Content:        "Target learning for injection recording.",
		Domain:         "dev",
		QualityScore:   0.8,
		InjectionCount: initialCount,
		CreatedAt:      time.Now(),
	})

	if err := db.RecordInjections(ctx, []string{"inject-target"}); err != nil {
		t.Fatalf("RecordInjections: %v", err)
	}

	var lastUsedAt sql.NullString
	var injCount int
	err := db.db.QueryRow(
		"SELECT last_used_at, injection_count FROM learnings WHERE id = ?", "inject-target",
	).Scan(&lastUsedAt, &injCount)
	if err != nil {
		t.Fatalf("querying inject-target: %v", err)
	}
	if !lastUsedAt.Valid || lastUsedAt.String == "" {
		t.Error("last_used_at must be non-NULL after RecordInjections")
	}
	if injCount != initialCount+1 {
		t.Errorf("injection_count: got %d, want %d", injCount, initialCount+1)
	}
}

// ---------------------------------------------------------------------------
// TestInsert_RequiresEmbedder
// ---------------------------------------------------------------------------

// TestInsert_RequiresEmbedder verifies that Insert returns ErrEmbedderRequired
// (wrapped) when an embedder is configured but embedding generation yields an
// empty vector — the strict guard preventing silent NULL-embedding rows.
func TestInsert_RequiresEmbedder(t *testing.T) {
	// Serve a 200 with empty values to trigger the empty-vector guard.
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, `{"embedding":{"values":[]}}`)
	}))
	defer srv.Close()

	embedder := &Embedder{
		apiKey:     "test-key",
		model:      "gemini-embedding-001",
		httpClient: srv.Client(),
		baseURL:    srv.URL,
	}

	db := newTestDB(t)
	l := Learning{
		ID:        "requires-embedder-test",
		Type:      TypeInsight,
		Content:   "content that must be embedded before storage",
		Domain:    "dev",
		CreatedAt: time.Now(),
	}

	err := db.Insert(context.Background(), l, embedder)
	if err == nil {
		t.Fatal("Insert with empty-vector embedder must return an error; got nil")
	}
	if !errors.Is(err, ErrEmbedderRequired) {
		t.Errorf("Insert error = %v; want errors.Is(err, ErrEmbedderRequired) == true", err)
	}

	// Row must not have been written.
	var n int
	if qErr := db.db.QueryRow("SELECT COUNT(*) FROM learnings WHERE id = ?", l.ID).Scan(&n); qErr != nil {
		t.Fatalf("count query: %v", qErr)
	}
	if n != 0 {
		t.Errorf("guarded row was written despite ErrEmbedderRequired (count=%d)", n)
	}
}
