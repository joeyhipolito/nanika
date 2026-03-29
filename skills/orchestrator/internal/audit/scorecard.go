package audit

import (
	"encoding/json"
	"fmt"
	"math"
	"strings"
	"time"
)

// MetricName identifies a scorecard axis.
type MetricName string

const (
	MetricDecomposition  MetricName = "decomposition"
	MetricPersonaFit     MetricName = "persona_fit"
	MetricSkillUsage     MetricName = "skill_usage"
	MetricOutputQuality  MetricName = "output_quality"
	MetricRuleCompliance MetricName = "rule_compliance"
	MetricOverall        MetricName = "overall"
)

// AllMetrics lists every tracked axis in display order.
var AllMetrics = []MetricName{
	MetricDecomposition,
	MetricPersonaFit,
	MetricSkillUsage,
	MetricOutputQuality,
	MetricRuleCompliance,
	MetricOverall,
}

// MetricLabel returns the human-readable label for a metric.
func MetricLabel(m MetricName) string {
	switch m {
	case MetricDecomposition:
		return "Decomposition"
	case MetricPersonaFit:
		return "Persona Fit"
	case MetricSkillUsage:
		return "Skill Usage"
	case MetricOutputQuality:
		return "Output Quality"
	case MetricRuleCompliance:
		return "Rule Compliance"
	case MetricOverall:
		return "Overall"
	default:
		return string(m)
	}
}

// DataPoint is a single metric observation in time.
type DataPoint struct {
	WorkspaceID string
	AuditedAt   time.Time
	Score       int
	Domain      string
}

// TrendLine summarizes a metric's trajectory over multiple audits.
type TrendLine struct {
	Metric  MetricName
	Points  []DataPoint
	Current int     // most recent score
	Average float64 // mean across all points
	Trend   string  // "improving", "stable", "declining"
	Delta   int     // score change from previous to current (signed)
	Min     int
	Max     int
}

// Regression represents a detected score drop linked to a specific audit.
type Regression struct {
	Metric      MetricName
	WorkspaceID string // workspace where regression occurred
	AuditedAt   time.Time
	PrevScore   int
	NewScore    int
	Drop        int // positive number: how much it dropped
	Domain      string
	TopIssues   []string // weaknesses from that audit
}

// ScorecardSummary is the complete output of scorecard analysis.
type ScorecardSummary struct {
	TotalAudits int
	DateRange   string // "2026-02-20 to 2026-02-28"
	Trends      []TrendLine
	Regressions []Regression
}

// BuildScorecard computes trend lines and detects regressions across audit reports.
// Reports should be in chronological order (oldest first).
func BuildScorecard(reports []AuditReport) *ScorecardSummary {
	if len(reports) == 0 {
		return &ScorecardSummary{}
	}

	summary := &ScorecardSummary{
		TotalAudits: len(reports),
	}

	// Date range
	first := reports[0].AuditedAt
	last := reports[len(reports)-1].AuditedAt
	summary.DateRange = fmt.Sprintf("%s to %s",
		first.Format("2006-01-02"), last.Format("2006-01-02"))

	// Build data points per metric
	for _, metric := range AllMetrics {
		var points []DataPoint
		for _, r := range reports {
			score := extractScore(r.Scorecard, metric)
			points = append(points, DataPoint{
				WorkspaceID: r.WorkspaceID,
				AuditedAt:   r.AuditedAt,
				Score:       score,
				Domain:      r.Domain,
			})
		}
		summary.Trends = append(summary.Trends, computeTrend(metric, points))
	}

	// Detect regressions
	summary.Regressions = detectRegressions(reports)

	return summary
}

// extractScore gets the score for a given metric from a scorecard.
func extractScore(s Scorecard, m MetricName) int {
	switch m {
	case MetricDecomposition:
		return s.DecompositionQuality
	case MetricPersonaFit:
		return s.PersonaFit
	case MetricSkillUsage:
		return s.SkillUtilization
	case MetricOutputQuality:
		return s.OutputQuality
	case MetricRuleCompliance:
		return s.RuleCompliance
	case MetricOverall:
		return s.Overall
	default:
		return 0
	}
}

// computeTrend builds a TrendLine from ordered data points.
func computeTrend(metric MetricName, points []DataPoint) TrendLine {
	t := TrendLine{
		Metric: metric,
		Points: points,
	}
	if len(points) == 0 {
		return t
	}

	t.Current = points[len(points)-1].Score
	t.Min = points[0].Score
	t.Max = points[0].Score
	sum := 0
	for _, p := range points {
		sum += p.Score
		if p.Score < t.Min {
			t.Min = p.Score
		}
		if p.Score > t.Max {
			t.Max = p.Score
		}
	}
	t.Average = float64(sum) / float64(len(points))

	if len(points) >= 2 {
		t.Delta = points[len(points)-1].Score - points[len(points)-2].Score
	}

	t.Trend = classifyTrend(points)
	return t
}

// classifyTrend determines direction using least-squares linear regression slope.
func classifyTrend(points []DataPoint) string {
	n := len(points)
	if n < 2 {
		return "stable"
	}

	var sumX, sumY, sumXY, sumX2 float64
	for i, p := range points {
		x := float64(i)
		y := float64(p.Score)
		sumX += x
		sumY += y
		sumXY += x * y
		sumX2 += x * x
	}
	nf := float64(n)
	denom := nf*sumX2 - sumX*sumX
	if denom == 0 {
		return "stable"
	}
	slope := (nf*sumXY - sumX*sumY) / denom

	// Threshold: slope of ±0.15 per audit counts as meaningful change
	if math.Abs(slope) < 0.15 {
		return "stable"
	}
	if slope > 0 {
		return "improving"
	}
	return "declining"
}

// detectRegressions finds significant score drops between consecutive audits.
func detectRegressions(reports []AuditReport) []Regression {
	if len(reports) < 2 {
		return nil
	}

	var regressions []Regression
	for i := 1; i < len(reports); i++ {
		prev := reports[i-1]
		curr := reports[i]

		for _, metric := range AllMetrics {
			prevScore := extractScore(prev.Scorecard, metric)
			currScore := extractScore(curr.Scorecard, metric)
			drop := prevScore - currScore

			if drop >= 1 {
				var issues []string
				issues = append(issues, curr.Evaluation.Weaknesses...)
				if len(issues) > 3 {
					issues = issues[:3]
				}

				regressions = append(regressions, Regression{
					Metric:      metric,
					WorkspaceID: curr.WorkspaceID,
					AuditedAt:   curr.AuditedAt,
					PrevScore:   prevScore,
					NewScore:    currScore,
					Drop:        drop,
					Domain:      curr.Domain,
					TopIssues:   issues,
				})
			}
		}
	}
	return regressions
}

// FormatScorecard renders the scorecard summary as terminal text.
func FormatScorecard(s *ScorecardSummary) string {
	var b strings.Builder

	b.WriteString("Audit Scorecard\n")
	b.WriteString(strings.Repeat("=", 60))
	b.WriteString("\n")
	b.WriteString(fmt.Sprintf("Audits: %d  |  Period: %s\n\n", s.TotalAudits, s.DateRange))

	if s.TotalAudits == 0 {
		b.WriteString("No audit reports found. Run `orchestrator audit` first.\n")
		return b.String()
	}

	// Trend table
	b.WriteString("Metric Trends\n")
	b.WriteString(strings.Repeat("-", 60))
	b.WriteString("\n")
	b.WriteString(fmt.Sprintf("  %-17s  %-4s  %-4s  %-7s  %-5s  %s\n",
		"Metric", "Cur", "Avg", "Range", "Delta", "Trend"))
	b.WriteString(fmt.Sprintf("  %-17s  %-4s  %-4s  %-7s  %-5s  %s\n",
		strings.Repeat("-", 17), "---", "---", "-----", "-----", "----------"))

	for _, t := range s.Trends {
		deltaStr := "  -"
		if len(t.Points) >= 2 {
			if t.Delta > 0 {
				deltaStr = fmt.Sprintf(" +%d", t.Delta)
			} else if t.Delta < 0 {
				deltaStr = fmt.Sprintf(" %d", t.Delta)
			} else {
				deltaStr = "  0"
			}
		}

		trendIcon := "="
		switch t.Trend {
		case "improving":
			trendIcon = "^"
		case "declining":
			trendIcon = "v"
		}

		b.WriteString(fmt.Sprintf("  %-17s  %d/5  %.1f  %d - %d  %s    %s %s\n",
			MetricLabel(t.Metric),
			t.Current,
			t.Average,
			t.Min, t.Max,
			deltaStr,
			trendIcon,
			t.Trend,
		))
	}
	b.WriteString("\n")

	// Sparklines
	maxSpark := 10
	count := s.TotalAudits
	if count > maxSpark {
		count = maxSpark
	}
	b.WriteString(fmt.Sprintf("Score History (last %d audits)\n", count))
	b.WriteString(strings.Repeat("-", 60))
	b.WriteString("\n")

	for _, t := range s.Trends {
		points := t.Points
		if len(points) > maxSpark {
			points = points[len(points)-maxSpark:]
		}
		spark := sparkline(points)
		b.WriteString(fmt.Sprintf("  %-17s  %s\n", MetricLabel(t.Metric), spark))
	}
	b.WriteString("\n")

	// Regressions
	if len(s.Regressions) > 0 {
		b.WriteString(fmt.Sprintf("Regressions Detected (%d)\n", len(s.Regressions)))
		b.WriteString(strings.Repeat("-", 60))
		b.WriteString("\n")
		for _, r := range s.Regressions {
			b.WriteString(fmt.Sprintf("  %s: %d -> %d (-%d) in %s\n",
				MetricLabel(r.Metric),
				r.PrevScore, r.NewScore, r.Drop,
				r.WorkspaceID,
			))
			for _, issue := range r.TopIssues {
				b.WriteString(fmt.Sprintf("    ! %s\n", issue))
			}
		}
	} else {
		b.WriteString("No regressions detected.\n")
	}

	return b.String()
}

// sparkline renders data points as a mini chart using block characters.
func sparkline(points []DataPoint) string {
	if len(points) == 0 {
		return ""
	}
	blocks := []string{" ", ".", ":", "#", "@"}
	var parts []string
	for _, p := range points {
		idx := p.Score - 1
		if idx < 0 {
			idx = 0
		}
		if idx > 4 {
			idx = 4
		}
		parts = append(parts, blocks[idx])
	}
	return "[" + strings.Join(parts, "") + "]"
}

// FormatScorecardJSON renders the scorecard as pretty-printed JSON.
func FormatScorecardJSON(s *ScorecardSummary) (string, error) {
	type jsonTrend struct {
		Metric  string  `json:"metric"`
		Current int     `json:"current"`
		Average float64 `json:"average"`
		Min     int     `json:"min"`
		Max     int     `json:"max"`
		Delta   int     `json:"delta"`
		Trend   string  `json:"trend"`
		History []int   `json:"history"`
	}

	type jsonRegression struct {
		Metric      string   `json:"metric"`
		WorkspaceID string   `json:"workspace_id"`
		AuditedAt   string   `json:"audited_at"`
		PrevScore   int      `json:"prev_score"`
		NewScore    int      `json:"new_score"`
		Drop        int      `json:"drop"`
		Domain      string   `json:"domain"`
		TopIssues   []string `json:"top_issues"`
	}

	type jsonScorecard struct {
		TotalAudits int              `json:"total_audits"`
		DateRange   string           `json:"date_range"`
		Trends      []jsonTrend      `json:"trends"`
		Regressions []jsonRegression `json:"regressions"`
	}

	out := jsonScorecard{
		TotalAudits: s.TotalAudits,
		DateRange:   s.DateRange,
	}

	for _, t := range s.Trends {
		jt := jsonTrend{
			Metric:  string(t.Metric),
			Current: t.Current,
			Average: math.Round(t.Average*10) / 10,
			Min:     t.Min,
			Max:     t.Max,
			Delta:   t.Delta,
			Trend:   t.Trend,
		}
		for _, p := range t.Points {
			jt.History = append(jt.History, p.Score)
		}
		out.Trends = append(out.Trends, jt)
	}

	for _, r := range s.Regressions {
		out.Regressions = append(out.Regressions, jsonRegression{
			Metric:      string(r.Metric),
			WorkspaceID: r.WorkspaceID,
			AuditedAt:   r.AuditedAt.Format(time.RFC3339),
			PrevScore:   r.PrevScore,
			NewScore:    r.NewScore,
			Drop:        r.Drop,
			Domain:      r.Domain,
			TopIssues:   r.TopIssues,
		})
	}

	data, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		return "", fmt.Errorf("marshaling scorecard: %w", err)
	}
	return string(data), nil
}
