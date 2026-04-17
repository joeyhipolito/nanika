package main

import (
	"testing"

	"github.com/joeyhipolito/nen/internal/scan"
)

func TestGyoSeverityForMetric(t *testing.T) {
	tests := []struct {
		name        string
		metric      string
		z           float64
		wantFlagged bool
		wantSev     scan.Severity
	}{
		// Noisy metric: failure_rate — z below noisy medium threshold, not flagged.
		{name: "failure_rate z=3.0 not flagged", metric: "failure_rate", z: 3.0, wantFlagged: false},
		// Noisy metric: failure_rate — z at noisy medium threshold, flagged as medium.
		{name: "failure_rate z=3.5 medium", metric: "failure_rate", z: 3.5, wantFlagged: true, wantSev: scan.SeverityMedium},
		// Noisy metric: retries — z at noisy high threshold, flagged as high.
		{name: "retries z=4.0 high", metric: "retries", z: 4.0, wantFlagged: true, wantSev: scan.SeverityHigh},
		// Global metric: cost_usd — z=2.5 clears global medium threshold unchanged.
		{name: "cost_usd z=2.5 medium", metric: "cost_usd", z: 2.5, wantFlagged: true, wantSev: scan.SeverityMedium},
		// Global metric: cost_usd — z below global threshold, not flagged.
		{name: "cost_usd z=2.4 not flagged", metric: "cost_usd", z: 2.4, wantFlagged: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sev, flagged := gyoSeverityForMetric(tt.metric, tt.z)
			if flagged != tt.wantFlagged {
				t.Errorf("gyoSeverityForMetric(%q, %v): flagged=%v, want %v", tt.metric, tt.z, flagged, tt.wantFlagged)
			}
			if flagged && sev != tt.wantSev {
				t.Errorf("gyoSeverityForMetric(%q, %v): severity=%q, want %q", tt.metric, tt.z, sev, tt.wantSev)
			}
		})
	}
}
