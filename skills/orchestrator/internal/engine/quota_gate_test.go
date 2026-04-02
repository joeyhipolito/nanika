package engine

// TestQuotaGate verifies the four key quota gate properties:
//
//  1. cache_read tokens are excluded from utilization when writing a snapshot
//  2. 30% actual usage does not trigger throttling
//  3. 95%+ genuine utilization triggers throttleBlock
//  4. RYU_THROTTLE_ENABLED=false bypasses the gate regardless of utilization
//  5. During peak hours, utilization is scaled by 1/0.7 (conservative 0.7x budget)

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	metricsdb "github.com/joeyhipolito/orchestrator-cli/internal/metrics"
)

// writePeakConfig writes a peak-hours.json to dir and returns the dir for use as HOME.
func writePeakConfig(t *testing.T, enabled bool) string {
	t.Helper()
	home := t.TempDir()
	alluka := filepath.Join(home, ".alluka")
	if err := os.MkdirAll(alluka, 0o755); err != nil {
		t.Fatalf("writePeakConfig: mkdir: %v", err)
	}
	content := fmt.Sprintf(
		`{"enabled":%v,"start_hour":0,"end_hour":24,"timezone":"UTC"}`,
		enabled,
	)
	if err := os.WriteFile(filepath.Join(alluka, "peak-hours.json"), []byte(content), 0o644); err != nil {
		t.Fatalf("writePeakConfig: write: %v", err)
	}
	return home
}

// seedSnap inserts a quota snapshot with the given Estimated5hUtil into db.
func seedSnap(t *testing.T, db *metricsdb.DB, missionID string, util float64) {
	t.Helper()
	snap := metricsdb.QuotaSnapshot{
		CapturedAt:      time.Now().UTC(),
		MissionID:       missionID,
		Estimated5hUtil: util,
	}
	if err := db.InsertQuotaSnapshot(context.Background(), snap); err != nil {
		t.Fatalf("seedSnap: %v", err)
	}
}

// quotaTestEngine returns a bare Engine for quota gate tests (no executor needed).
func quotaTestEngine(t *testing.T, cfgDir string, force bool) *Engine {
	t.Helper()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)
	ws := &core.Workspace{ID: "ws-quota-test", Path: t.TempDir(), Domain: "dev"}
	return New(ws, &core.OrchestratorConfig{Force: force}, nil, nil, "")
}

// TestQuotaGate_CacheReadExcludedFromUtilization verifies that cache_read tokens
// are subtracted from tokens_in before computing Estimated5hUtil. A mission whose
// token_in is almost entirely cache_read should produce near-zero utilization,
// not a false high utilization that would trigger throttling.
func TestQuotaGate_CacheReadExcludedFromUtilization(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	// 45M tokens_in, 43.5M of which are cache_read → effective_in = 1.5M
	// out = 0. budget = 50M (default).
	// expected util = 1.5M / 50M = 3%.
	m := MissionMetrics{
		WorkspaceID:        "ws-cache-read-test",
		Domain:             "dev",
		Task:               "cache_read exclusion test",
		StartedAt:          time.Now().Add(-time.Minute),
		FinishedAt:         time.Now(),
		DurationSec:        60,
		PhasesTotal:        1,
		PhasesCompleted:    1,
		Status:             "success",
		TokensInTotal:      45_000_000,
		TokensOutTotal:     0,
		TokensCacheReadTotal: 43_500_000,
	}

	if err := recordQuotaSnapshotDB(m); err != nil {
		t.Fatalf("recordQuotaSnapshotDB: %v", err)
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	snaps, err := db.GetRecentSnapshots(context.Background(), time.Hour)
	if err != nil {
		t.Fatalf("GetRecentSnapshots: %v", err)
	}
	if len(snaps) == 0 {
		t.Fatal("want at least one snapshot, got none")
	}

	util := snaps[len(snaps)-1].Estimated5hUtil
	// effectiveIn=1.5M, out=0, budget=50M → 3%
	const wantMax = 0.10 // must be well below throttle threshold
	if util > wantMax {
		t.Errorf("Estimated5hUtil = %.1f%%, want < %.1f%% (cache_read should be excluded)",
			util*100, wantMax*100)
	}
	// Sanity: must be non-zero (1.5M effective tokens were real).
	if util <= 0 {
		t.Errorf("Estimated5hUtil = %.4f, want > 0 (effective_in should count non-cache tokens)", util)
	}
}

// TestQuotaGate_ThrottleAction tests checkQuotaGate action selection across
// utilization levels, including the 30%-usage-no-throttle and 95%-block cases.
func TestQuotaGate_ThrottleAction(t *testing.T) {
	tests := []struct {
		name       string
		util       float64
		phaseP0    bool
		wantAction throttleAction
	}{
		{
			name:       "30% usage does not trigger throttling",
			util:       0.30,
			wantAction: throttleNormal,
		},
		{
			name:       "59.9% usage does not trigger throttling",
			util:       0.599,
			wantAction: throttleNormal,
		},
		{
			name:       "60% usage downgrades non-P0 to sonnet",
			util:       0.60,
			wantAction: throttleForceSonnet,
		},
		{
			name:       "60% usage does not downgrade P0",
			util:       0.60,
			phaseP0:    true,
			wantAction: throttleNormal,
		},
		{
			name:       "80% usage skips non-P0",
			util:       0.80,
			wantAction: throttleSkip,
		},
		{
			name:       "80% usage does not skip P0",
			util:       0.80,
			phaseP0:    true,
			wantAction: throttleNormal,
		},
		{
			name:       "genuine 95% utilization blocks all phases",
			util:       0.95,
			wantAction: throttleBlock,
		},
		{
			name:       "genuine 95% utilization blocks P0 phase too",
			util:       0.95,
			phaseP0:    true,
			wantAction: throttleBlock,
		},
		{
			name:       "100% utilization blocks all phases",
			util:       1.00,
			wantAction: throttleBlock,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfgDir := t.TempDir()
			// Isolate HOME so peak.LoadConfig inside checkQuotaGate finds no
			// peak-hours.json — prevents the 0.7x scaling from flaking these tests
			// when run during real peak hours.
			t.Setenv("HOME", writePeakConfig(t, false))
			e := quotaTestEngine(t, cfgDir, false)

			db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
			if err != nil {
				t.Fatalf("InitDB: %v", err)
			}
			t.Cleanup(func() { db.Close() })

			seedSnap(t, db, "mission-gate-test", tt.util)

			priority := ""
			if tt.phaseP0 {
				priority = "P0"
			}
			phase := &core.Phase{
				ID:       "test-phase",
				Name:     "test",
				Priority: priority,
			}

			action, got := e.checkQuotaGate(phase)
			if action != tt.wantAction {
				t.Errorf("checkQuotaGate action = %v (util %.1f%%), want %v",
					action, got*100, tt.wantAction)
			}
		})
	}
}

// TestQuotaGate_DisabledByEnvVar verifies that RYU_THROTTLE_ENABLED=false
// bypasses the quota gate even when utilization would normally cause blocking.
func TestQuotaGate_DisabledByEnvVar(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("RYU_THROTTLE_ENABLED", "false")
	e := quotaTestEngine(t, cfgDir, false)

	// Seed a snapshot that would normally trigger throttleBlock.
	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	seedSnap(t, db, "mission-disabled-test", 0.99)

	phase := &core.Phase{ID: "test-phase", Name: "test"}
	action, util := e.checkQuotaGate(phase)
	if action != throttleNormal {
		t.Errorf("RYU_THROTTLE_ENABLED=false: got action=%v util=%.1f%%, want throttleNormal",
			action, util*100)
	}
	if util != 0 {
		t.Errorf("RYU_THROTTLE_ENABLED=false: got util=%.4f, want 0 (gate never read DB)", util)
	}
}

// TestQuotaGate_PeakHours_BudgetScaling verifies the 0.7x conservative budget
// that is applied during peak hours.
//
// Key property: a raw utilization of 50% (which would be throttleNormal off-peak)
// becomes 50%/0.7 ≈ 71.4% during peak — crossing the 60% throttleForceSonnet
// threshold.
//
// The "peak active" sub-test uses a peak config window of 0–24 UTC so peak is
// active on any weekday regardless of wall-clock time. The test skips on weekends
// because the peak package intentionally excludes Saturday and Sunday.
func TestQuotaGate_PeakHours_BudgetScaling(t *testing.T) {
	t.Run("off-peak: 50% raw stays throttleNormal", func(t *testing.T) {
		cfgDir := t.TempDir()
		// Set HOME to a dir with enabled=false — peak never fires.
		t.Setenv("HOME", writePeakConfig(t, false))
		e := quotaTestEngine(t, cfgDir, false)

		db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
		if err != nil {
			t.Fatalf("InitDB: %v", err)
		}
		t.Cleanup(func() { db.Close() })
		seedSnap(t, db, "mission-off-peak", 0.50)

		phase := &core.Phase{ID: "test-phase", Name: "test", Priority: ""}
		action, got := e.checkQuotaGate(phase)
		if action != throttleNormal {
			t.Errorf("off-peak 50%% util: got action=%v (util=%.1f%%), want throttleNormal",
				action, got*100)
		}
	})

	t.Run("peak active: 50% raw scales to 71.4% effective → throttleForceSonnet", func(t *testing.T) {
		if wd := time.Now().UTC().Weekday(); wd == time.Saturday || wd == time.Sunday {
			t.Skip("peak package excludes weekends; run on a weekday")
		}
		cfgDir := t.TempDir()
		// enabled=true, all UTC hours covered → IsPeak returns true on any weekday.
		t.Setenv("HOME", writePeakConfig(t, true))
		e := quotaTestEngine(t, cfgDir, false)

		db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
		if err != nil {
			t.Fatalf("InitDB: %v", err)
		}
		t.Cleanup(func() { db.Close() })
		// 50% raw / 0.7 ≈ 71.4% effective → above 60% threshold → throttleForceSonnet.
		seedSnap(t, db, "mission-peak", 0.50)

		phase := &core.Phase{ID: "test-phase", Name: "test", Priority: ""}
		action, _ := e.checkQuotaGate(phase)
		if action != throttleForceSonnet {
			t.Errorf("peak 50%% raw (≈71.4%% effective): got action=%v, want throttleForceSonnet", action)
		}
	})

	t.Run("peak active: P0 phase not skipped even at 80% effective", func(t *testing.T) {
		if wd := time.Now().UTC().Weekday(); wd == time.Saturday || wd == time.Sunday {
			t.Skip("peak package excludes weekends; run on a weekday")
		}
		cfgDir := t.TempDir()
		t.Setenv("HOME", writePeakConfig(t, true))
		e := quotaTestEngine(t, cfgDir, false)

		db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
		if err != nil {
			t.Fatalf("InitDB: %v", err)
		}
		t.Cleanup(func() { db.Close() })
		// 58% raw / 0.7 ≈ 82.9% effective → above 80% threshold, but P0 → throttleNormal.
		seedSnap(t, db, "mission-peak-p0", 0.58)

		phase := &core.Phase{ID: "test-phase", Name: "test", Priority: "P0"}
		action, _ := e.checkQuotaGate(phase)
		if action != throttleNormal {
			t.Errorf("peak P0 phase at 82.9%% effective: got action=%v, want throttleNormal", action)
		}
	})
}
