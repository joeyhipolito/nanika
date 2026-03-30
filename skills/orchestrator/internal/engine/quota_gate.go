package engine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	metricsdb "github.com/joeyhipolito/orchestrator-cli/internal/metrics"
)

// errQuotaGateSkip is returned by executePhase when the quota gate determines
// the phase should be skipped. The calling loop must NOT mark the phase as
// failed — the phase status is already set to StatusSkipped.
var errQuotaGateSkip = errors.New("quota gate: phase skipped due to high utilization")

// throttleAction is the action the quota gate requires for a phase.
type throttleAction int

const (
	throttleNormal      throttleAction = iota // proceed normally
	throttleForceSonnet                       // downgrade model to sonnet
	throttleSkip                              // skip this non-P0 phase
	throttleBlock                             // block all phases (critical)
)

// checkQuotaGate reads the latest quota snapshot and returns the required
// throttle action and the observed 5h utilization ratio.
//
// Thresholds:
//
//	< 60%  → throttleNormal
//	60–80% → throttleForceSonnet (non-P0 only)
//	80–95% → throttleSkip (non-P0 only)
//	≥ 95%  → throttleBlock (all phases)
//
// The gate is a no-op when:
//   - RYU_THROTTLE_ENABLED=false
//   - e.config.Force is true
//   - no quota snapshots exist in the DB (first mission; can't measure yet)
//   - the DB is unreachable (fail-open to avoid blocking legitimate work)
func (e *Engine) checkQuotaGate(phase *core.Phase) (throttleAction, float64) {
	if os.Getenv("RYU_THROTTLE_ENABLED") == "false" {
		return throttleNormal, 0
	}
	if e.config.Force {
		fmt.Printf("[ryu] quota gate bypassed by --force (phase=%s)\n", phaseRuntimeID(phase))
		return throttleNormal, 0
	}

	db, err := metricsdb.InitDB("")
	if err != nil {
		fmt.Fprintf(os.Stderr, "[ryu] quota gate: db open failed: %v\n", err)
		return throttleNormal, 0
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	// Pull the most recent snapshot from the last hour (enough resolution; avoids
	// a full table scan while still capturing any mission that just completed).
	snaps, err := db.GetRecentSnapshots(ctx, time.Hour)
	if err != nil || len(snaps) == 0 {
		// No history yet or DB error → fail open.
		return throttleNormal, 0
	}

	latest := snaps[len(snaps)-1]
	util := latest.Estimated5hUtil
	isP0 := phase.Priority == "P0"
	phaseID := phaseRuntimeID(phase)

	switch {
	case util >= 0.95:
		fmt.Printf("[ryu] CRITICAL: 5h utilization %.1f%% >= 95%% — blocking ALL phases (phase=%s)\n",
			util*100, phaseID)
		return throttleBlock, util

	case util >= 0.80 && !isP0:
		fmt.Printf("[ryu] WARNING: 5h utilization %.1f%% >= 80%% — skipping non-P0 phase %s\n",
			util*100, phaseID)
		return throttleSkip, util

	case util >= 0.60 && !isP0:
		fmt.Printf("[ryu] INFO: 5h utilization %.1f%% >= 60%% — downgrading non-P0 phase %s to sonnet\n",
			util*100, phaseID)
		return throttleForceSonnet, util

	default:
		if e.config.Verbose {
			fmt.Printf("[ryu] quota gate ok: 5h utilization %.1f%% (phase=%s)\n", util*100, phaseID)
		}
		return throttleNormal, util
	}
}
