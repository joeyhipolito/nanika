package audit

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// contains is a test helper for substring matching.
func contains(s, substr string) bool { return strings.Contains(s, substr) }

func TestBuildScorecardEmpty(t *testing.T) {
	s := BuildScorecard(nil)
	if s.TotalAudits != 0 {
		t.Errorf("expected 0 audits, got %d", s.TotalAudits)
	}
	if len(s.Trends) != 0 {
		t.Errorf("expected no trends, got %d", len(s.Trends))
	}
}

func TestBuildScorecardSingle(t *testing.T) {
	reports := []AuditReport{
		makeReport("ws-1", "dev", time.Date(2026, 2, 20, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 4, PersonaFit: 3, SkillUtilization: 2, OutputQuality: 4, RuleCompliance: 3, Overall: 3}),
	}
	s := BuildScorecard(reports)
	if s.TotalAudits != 1 {
		t.Errorf("expected 1 audit, got %d", s.TotalAudits)
	}
	if len(s.Trends) != 6 {
		t.Fatalf("expected 6 trends, got %d", len(s.Trends))
	}

	// Check decomposition trend
	dt := s.Trends[0]
	if dt.Current != 4 {
		t.Errorf("decomposition current: got %d, want 4", dt.Current)
	}
	if dt.Average != 4.0 {
		t.Errorf("decomposition avg: got %.1f, want 4.0", dt.Average)
	}
	if dt.Trend != "stable" {
		t.Errorf("single-point trend: got %q, want stable", dt.Trend)
	}
	if dt.Delta != 0 {
		t.Errorf("single-point delta: got %d, want 0", dt.Delta)
	}

	// No regressions with a single report
	if len(s.Regressions) != 0 {
		t.Errorf("expected 0 regressions, got %d", len(s.Regressions))
	}
}

func TestBuildScorecardTrends(t *testing.T) {
	reports := []AuditReport{
		makeReport("ws-1", "dev", time.Date(2026, 2, 20, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 2, PersonaFit: 4, SkillUtilization: 3, OutputQuality: 3, RuleCompliance: 3, Overall: 3}),
		makeReport("ws-2", "dev", time.Date(2026, 2, 22, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 3, PersonaFit: 4, SkillUtilization: 3, OutputQuality: 3, RuleCompliance: 3, Overall: 3}),
		makeReport("ws-3", "dev", time.Date(2026, 2, 24, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 4, PersonaFit: 3, SkillUtilization: 3, OutputQuality: 4, RuleCompliance: 3, Overall: 3}),
		makeReport("ws-4", "dev", time.Date(2026, 2, 26, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 5, PersonaFit: 2, SkillUtilization: 3, OutputQuality: 4, RuleCompliance: 4, Overall: 3}),
	}

	s := BuildScorecard(reports)

	// Decomposition: 2, 3, 4, 5 — clearly improving
	dt := findTrend(s.Trends, MetricDecomposition)
	if dt.Trend != "improving" {
		t.Errorf("decomposition trend: got %q, want improving", dt.Trend)
	}
	if dt.Delta != 1 {
		t.Errorf("decomposition delta: got %d, want 1", dt.Delta)
	}
	if dt.Min != 2 || dt.Max != 5 {
		t.Errorf("decomposition range: got %d-%d, want 2-5", dt.Min, dt.Max)
	}

	// Persona fit: 4, 4, 3, 2 — clearly declining
	pf := findTrend(s.Trends, MetricPersonaFit)
	if pf.Trend != "declining" {
		t.Errorf("persona_fit trend: got %q, want declining", pf.Trend)
	}

	// Skill usage: 3, 3, 3, 3 — stable
	su := findTrend(s.Trends, MetricSkillUsage)
	if su.Trend != "stable" {
		t.Errorf("skill_usage trend: got %q, want stable", su.Trend)
	}
}

func TestDetectRegressions(t *testing.T) {
	reports := []AuditReport{
		makeReport("ws-1", "dev", time.Date(2026, 2, 20, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 4, PersonaFit: 4, SkillUtilization: 3, OutputQuality: 4, RuleCompliance: 4, Overall: 3}),
		makeReport("ws-2", "dev", time.Date(2026, 2, 22, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 2, PersonaFit: 4, SkillUtilization: 3, OutputQuality: 4, RuleCompliance: 4, Overall: 3}),
	}
	// Add weaknesses to ws-2 for regression context
	reports[1].Evaluation.Weaknesses = []string{"Bad decomposition", "Too many phases"}

	s := BuildScorecard(reports)

	if len(s.Regressions) == 0 {
		t.Fatal("expected at least one regression")
	}

	// Should detect decomposition regression
	found := false
	for _, r := range s.Regressions {
		if r.Metric == MetricDecomposition {
			found = true
			if r.PrevScore != 4 || r.NewScore != 2 || r.Drop != 2 {
				t.Errorf("regression: got %d->%d (-%d), want 4->2 (-2)",
					r.PrevScore, r.NewScore, r.Drop)
			}
			if r.WorkspaceID != "ws-2" {
				t.Errorf("regression workspace: got %q, want ws-2", r.WorkspaceID)
			}
			if len(r.TopIssues) != 2 {
				t.Errorf("regression issues: got %d, want 2", len(r.TopIssues))
			}
		}
	}
	if !found {
		t.Error("decomposition regression not detected")
	}
}

func TestClassifyTrend(t *testing.T) {
	tests := []struct {
		name   string
		scores []int
		want   string
	}{
		{"single", []int{3}, "stable"},
		{"flat", []int{3, 3, 3, 3}, "stable"},
		{"rising", []int{1, 2, 3, 4, 5}, "improving"},
		{"falling", []int{5, 4, 3, 2, 1}, "declining"},
		{"slight_rise", []int{3, 3, 3, 4}, "improving"},
		{"slight_fall", []int{4, 3, 3, 3}, "declining"},
		{"noisy_stable", []int{3, 4, 3, 3}, "stable"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var points []DataPoint
			for _, s := range tt.scores {
				points = append(points, DataPoint{Score: s})
			}
			got := classifyTrend(points)
			if got != tt.want {
				t.Errorf("classifyTrend(%v) = %q, want %q", tt.scores, got, tt.want)
			}
		})
	}
}

func TestExtractScore(t *testing.T) {
	s := Scorecard{
		DecompositionQuality: 1,
		PersonaFit:           2,
		SkillUtilization:     3,
		OutputQuality:        4,
		RuleCompliance:       5,
		Overall:              3,
	}
	tests := []struct {
		metric MetricName
		want   int
	}{
		{MetricDecomposition, 1},
		{MetricPersonaFit, 2},
		{MetricSkillUsage, 3},
		{MetricOutputQuality, 4},
		{MetricRuleCompliance, 5},
		{MetricOverall, 3},
		{MetricName("unknown"), 0},
	}
	for _, tt := range tests {
		got := extractScore(s, tt.metric)
		if got != tt.want {
			t.Errorf("extractScore(%s) = %d, want %d", tt.metric, got, tt.want)
		}
	}
}

func TestSparkline(t *testing.T) {
	tests := []struct {
		scores []int
		want   string
	}{
		{nil, ""},
		{[]int{1}, "[ ]"},
		{[]int{1, 2, 3, 4, 5}, "[ .:#@]"},
		{[]int{3, 3, 3}, "[:::]"},
		{[]int{5, 5, 5}, "[@@@]"},
	}
	for _, tt := range tests {
		var points []DataPoint
		for _, s := range tt.scores {
			points = append(points, DataPoint{Score: s})
		}
		got := sparkline(points)
		if got != tt.want {
			t.Errorf("sparkline(%v) = %q, want %q", tt.scores, got, tt.want)
		}
	}
}

func TestFormatScorecard(t *testing.T) {
	reports := []AuditReport{
		makeReport("ws-1", "dev", time.Date(2026, 2, 20, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 3, PersonaFit: 3, SkillUtilization: 3, OutputQuality: 3, RuleCompliance: 3, Overall: 3}),
		makeReport("ws-2", "dev", time.Date(2026, 2, 22, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 4, PersonaFit: 4, SkillUtilization: 4, OutputQuality: 4, RuleCompliance: 4, Overall: 4}),
	}
	s := BuildScorecard(reports)
	text := FormatScorecard(s)

	for _, want := range []string{"Audit Scorecard", "Metric Trends", "Decomposition", "Score History", "regressions"} {
		if !contains(text, want) {
			t.Errorf("formatted text missing %q", want)
		}
	}
}

func TestFormatScorecardJSON(t *testing.T) {
	reports := []AuditReport{
		makeReport("ws-1", "dev", time.Date(2026, 2, 20, 0, 0, 0, 0, time.UTC),
			Scorecard{DecompositionQuality: 3, PersonaFit: 3, SkillUtilization: 3, OutputQuality: 3, RuleCompliance: 3, Overall: 3}),
	}
	s := BuildScorecard(reports)
	out, err := FormatScorecardJSON(s)
	if err != nil {
		t.Fatal(err)
	}

	var check map[string]interface{}
	if err := json.Unmarshal([]byte(out), &check); err != nil {
		t.Fatalf("output is not valid JSON: %v", err)
	}
	if check["total_audits"].(float64) != 1 {
		t.Error("total_audits should be 1")
	}
}

func TestSaveAndLoadReports(t *testing.T) {
	// Use a temp dir to avoid touching the real audits file
	dir := t.TempDir()
	path := filepath.Join(dir, "audits.jsonl")

	// Monkey-patch the store path for this test
	origFunc := auditStorePathFunc
	auditStorePathFunc = func() (string, error) { return path, nil }
	defer func() { auditStorePathFunc = origFunc }()

	r1 := makeReport("ws-1", "dev", time.Date(2026, 2, 20, 10, 0, 0, 0, time.UTC),
		Scorecard{DecompositionQuality: 4, PersonaFit: 3, SkillUtilization: 2, OutputQuality: 4, RuleCompliance: 3, Overall: 3})
	r2 := makeReport("ws-2", "personal", time.Date(2026, 2, 22, 10, 0, 0, 0, time.UTC),
		Scorecard{DecompositionQuality: 5, PersonaFit: 4, SkillUtilization: 3, OutputQuality: 5, RuleCompliance: 4, Overall: 4})

	if err := SaveReport(&r1); err != nil {
		t.Fatalf("SaveReport r1: %v", err)
	}
	if err := SaveReport(&r2); err != nil {
		t.Fatalf("SaveReport r2: %v", err)
	}

	reports, err := LoadReports()
	if err != nil {
		t.Fatalf("LoadReports: %v", err)
	}
	if len(reports) != 2 {
		t.Fatalf("expected 2 reports, got %d", len(reports))
	}
	if reports[0].WorkspaceID != "ws-1" {
		t.Errorf("first report workspace: got %q, want ws-1", reports[0].WorkspaceID)
	}
	if reports[1].WorkspaceID != "ws-2" {
		t.Errorf("second report workspace: got %q, want ws-2", reports[1].WorkspaceID)
	}
	if reports[1].Scorecard.Overall != 4 {
		t.Errorf("second report overall: got %d, want 4", reports[1].Scorecard.Overall)
	}
}

func TestLoadReportsEmpty(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "nonexistent", "audits.jsonl")

	origFunc := auditStorePathFunc
	auditStorePathFunc = func() (string, error) { return path, nil }
	defer func() { auditStorePathFunc = origFunc }()

	reports, err := LoadReports()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if reports != nil {
		t.Fatalf("expected nil, got %v", reports)
	}
}

func TestFormatScorecardEmpty(t *testing.T) {
	s := BuildScorecard(nil)
	text := FormatScorecard(s)
	if !contains(text, "No audit reports found") {
		t.Error("empty scorecard should say no reports found")
	}
}

func TestRegressionTopIssuesCapped(t *testing.T) {
	r1 := makeReport("ws-1", "dev", time.Date(2026, 2, 20, 0, 0, 0, 0, time.UTC),
		Scorecard{DecompositionQuality: 5, PersonaFit: 5, SkillUtilization: 5, OutputQuality: 5, RuleCompliance: 5, Overall: 5})
	r2 := makeReport("ws-2", "dev", time.Date(2026, 2, 22, 0, 0, 0, 0, time.UTC),
		Scorecard{DecompositionQuality: 1, PersonaFit: 1, SkillUtilization: 1, OutputQuality: 1, RuleCompliance: 1, Overall: 1})
	r2.Evaluation.Weaknesses = []string{"w1", "w2", "w3", "w4", "w5"}

	s := BuildScorecard([]AuditReport{r1, r2})
	for _, r := range s.Regressions {
		if len(r.TopIssues) > 3 {
			t.Errorf("regression %s has %d issues, want max 3", r.Metric, len(r.TopIssues))
		}
	}
}

// helpers

func makeReport(wsID, domain string, auditedAt time.Time, sc Scorecard) AuditReport {
	return AuditReport{
		WorkspaceID: wsID,
		Task:        "Test task for " + wsID,
		Domain:      domain,
		Status:      "completed",
		AuditedAt:   auditedAt,
		Scorecard:   sc,
		Evaluation: MissionEvaluation{
			Summary: "Test evaluation",
		},
	}
}

func findTrend(trends []TrendLine, metric MetricName) TrendLine {
	for _, t := range trends {
		if t.Metric == metric {
			return t
		}
	}
	return TrendLine{}
}

// Ensure audits.jsonl isn't touched during normal test runs.
func TestMain(m *testing.M) {
	// Override the store path for all tests in this package
	// unless a specific test sets its own override.
	os.Exit(m.Run())
}
