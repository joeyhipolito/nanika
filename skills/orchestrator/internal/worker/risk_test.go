package worker

import (
	"testing"
)

// ---------------------------------------------------------------------------
// ClassifyToolRisk
// ---------------------------------------------------------------------------

func TestClassifyToolRisk_LowTools(t *testing.T) {
	low := []string{"Read", "Glob", "Grep", "WebSearch", "WebFetch", "TaskOutput"}
	for _, tool := range low {
		t.Run(tool, func(t *testing.T) {
			if got := ClassifyToolRisk(tool); got != RiskLow {
				t.Errorf("ClassifyToolRisk(%q) = %q; want %q", tool, got, RiskLow)
			}
		})
	}
}

func TestClassifyToolRisk_MediumTools(t *testing.T) {
	medium := []string{"Bash", "Edit", "Write", "TodoWrite", "NotebookEdit"}
	for _, tool := range medium {
		t.Run(tool, func(t *testing.T) {
			if got := ClassifyToolRisk(tool); got != RiskMedium {
				t.Errorf("ClassifyToolRisk(%q) = %q; want %q", tool, got, RiskMedium)
			}
		})
	}
}

func TestClassifyToolRisk_HighTools(t *testing.T) {
	high := []string{"Agent"}
	for _, tool := range high {
		t.Run(tool, func(t *testing.T) {
			if got := ClassifyToolRisk(tool); got != RiskHigh {
				t.Errorf("ClassifyToolRisk(%q) = %q; want %q", tool, got, RiskHigh)
			}
		})
	}
}

func TestClassifyToolRisk_UnknownDefaultsMedium(t *testing.T) {
	unknown := []string{"", "SomeFutureTool", "XYZ"}
	for _, tool := range unknown {
		t.Run(tool, func(t *testing.T) {
			if got := ClassifyToolRisk(tool); got != RiskMedium {
				t.Errorf("ClassifyToolRisk(%q) = %q; want %q (unknown defaults to MEDIUM)", tool, got, RiskMedium)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// LowRiskTools
// ---------------------------------------------------------------------------

func TestLowRiskTools_AllClassifiedLow(t *testing.T) {
	// Every tool returned by LowRiskTools must classify as RiskLow.
	for _, tool := range LowRiskTools() {
		if got := ClassifyToolRisk(tool); got != RiskLow {
			t.Errorf("LowRiskTools contains %q but ClassifyToolRisk returns %q", tool, got)
		}
	}
}

func TestLowRiskTools_NonEmpty(t *testing.T) {
	if len(LowRiskTools()) == 0 {
		t.Error("LowRiskTools must return at least one tool")
	}
}

func TestLowRiskTools_Deterministic(t *testing.T) {
	// Multiple calls must return the same slice content.
	first := LowRiskTools()
	second := LowRiskTools()
	if len(first) != len(second) {
		t.Fatalf("LowRiskTools length differs between calls: %d vs %d", len(first), len(second))
	}
	for i := range first {
		if first[i] != second[i] {
			t.Errorf("LowRiskTools[%d]: first=%q second=%q", i, first[i], second[i])
		}
	}
}

func TestLowRiskTools_Sorted(t *testing.T) {
	tools := LowRiskTools()
	for i := 1; i < len(tools); i++ {
		if tools[i] < tools[i-1] {
			t.Errorf("LowRiskTools not sorted: %q < %q at index %d", tools[i], tools[i-1], i)
		}
	}
}

// ---------------------------------------------------------------------------
// RiskLevel constants
// ---------------------------------------------------------------------------

func TestRiskLevel_StringValues(t *testing.T) {
	cases := []struct {
		level RiskLevel
		want  string
	}{
		{RiskLow, "LOW"},
		{RiskMedium, "MEDIUM"},
		{RiskHigh, "HIGH"},
	}
	for _, tc := range cases {
		if string(tc.level) != tc.want {
			t.Errorf("RiskLevel string = %q; want %q", tc.level, tc.want)
		}
	}
}
