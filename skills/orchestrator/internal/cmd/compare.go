package cmd

import (
	"bufio"
	"database/sql"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/spf13/cobra"
	_ "modernc.org/sqlite"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

func init() {
	compareCmd := &cobra.Command{
		Use:   "compare",
		Short: "Compare v1 vs v2 orchestrator performance metrics",
		RunE:  runCompare,
	}
	compareCmd.Flags().String("domain", "", "filter by domain (dev, creative, personal, work, academic)")
	compareCmd.Flags().Int("days", 0, "limit to last N days (0 = all time)")

	rootCmd.AddCommand(compareCmd)
}

// ── v1 data structures ────────────────────────────────────────────────────────

type v1Mission struct {
	MissionID       string
	Domain          string
	StartedAt       time.Time
	DurationSeconds float64
	Status          string // completed, failed, crashed
	TotalPhases     int
	CompletedPhases int
	FailedPhases    int
}

type v1Phase struct {
	MissionID       string
	AgentPersona    string
	DurationSeconds float64
	Status          string // completed, failed, skipped
}

// ── v2 data structures ────────────────────────────────────────────────────────

type v2Entry struct {
	WorkspaceID        string    `json:"workspace_id"`
	Domain             string    `json:"domain"`
	Task               string    `json:"task"`
	StartedAt          time.Time `json:"started_at"`
	DurationSec        int       `json:"duration_s"`
	PhasesTotal        int       `json:"phases_total"`
	PhasesCompleted    int       `json:"phases_completed"`
	PhasesFailed       int       `json:"phases_failed"`
	PhasesSkipped      int       `json:"phases_skipped"`
	LearningsRetrieved int       `json:"learnings_retrieved"`
	RetriesTotal       int       `json:"retries_total"`
	GateFailures       int       `json:"gate_failures"`
	OutputLenTotal     int       `json:"output_len_total"`
	Status             string    `json:"status"` // success, failure, partial
	Phases             []struct {
		DurationS  int    `json:"duration_s"`
		Status     string `json:"status"`
		GatePassed bool   `json:"gate_passed"`
		OutputLen  int    `json:"output_len"`
	} `json:"phases"`
}

// ── aggregated stats ──────────────────────────────────────────────────────────

type stats struct {
	Missions        int
	Successes       int
	Failures        int
	Partials        int
	TotalDurS       float64
	Phases          int
	PhasesCompleted int
	PhasesFailed    int
	// v2-specific
	LearningsRetrieved int
	RetriesTotal       int
	GateFailures       int
	OutputLenTotal     int
	PhaseDurations     []float64
}

func (s *stats) successRate() float64 {
	if s.Missions == 0 {
		return 0
	}
	return float64(s.Successes) / float64(s.Missions) * 100
}

func (s *stats) phaseSuccessRate() float64 {
	if s.Phases == 0 {
		return 0
	}
	return float64(s.PhasesCompleted) / float64(s.Phases) * 100
}

func (s *stats) avgMissionDurS() float64 {
	if s.Missions == 0 {
		return 0
	}
	return s.TotalDurS / float64(s.Missions)
}

func (s *stats) avgPhaseDurS() float64 {
	if len(s.PhaseDurations) == 0 {
		return 0
	}
	sum := 0.0
	for _, d := range s.PhaseDurations {
		sum += d
	}
	return sum / float64(len(s.PhaseDurations))
}

func (s *stats) avgLearningsRetrieved() float64 {
	if s.Missions == 0 {
		return 0
	}
	return float64(s.LearningsRetrieved) / float64(s.Missions)
}

func (s *stats) avgRetriesPerMission() float64 {
	if s.Missions == 0 {
		return 0
	}
	return float64(s.RetriesTotal) / float64(s.Missions)
}

func (s *stats) avgGateFailuresPerMission() float64 {
	if s.Missions == 0 {
		return 0
	}
	return float64(s.GateFailures) / float64(s.Missions)
}

func (s *stats) avgOutputLenPerPhase() float64 {
	if s.Phases == 0 {
		return 0
	}
	return float64(s.OutputLenTotal) / float64(s.Phases)
}

// ── main command ──────────────────────────────────────────────────────────────

func runCompare(cmd *cobra.Command, args []string) error {
	domain, _ := cmd.Flags().GetString("domain")
	days, _ := cmd.Flags().GetInt("days")

	cutoff := time.Time{}
	if days > 0 {
		cutoff = time.Now().AddDate(0, 0, -days)
	}

	v1, err := loadV1Stats(domain, cutoff)
	if err != nil {
		fmt.Printf("⚠  v1 data unavailable: %v\n\n", err)
		v1 = &stats{}
	}

	v2, err := loadV2Stats(domain, cutoff)
	if err != nil {
		fmt.Printf("⚠  v2 data unavailable: %v\n\n", err)
		v2 = &stats{}
	}

	printReport(v1, v2, domain, days)
	return nil
}

func loadV1Stats(domain string, cutoff time.Time) (*stats, error) {
	base, err := config.Dir()
	if err != nil {
		return nil, fmt.Errorf("config dir: %w", err)
	}
	dbPath := filepath.Join(base, "learnings.db")

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		return nil, fmt.Errorf("open db: %w", err)
	}
	defer db.Close()

	// Note: v1 mission_metrics.status is always "running" (RecordMissionEnd was never called).
	// We infer mission-level status from phase_metrics via a JOIN.

	var conds []string
	var args []any
	if domain != "" {
		conds = append(conds, "m.domain = ?")
		args = append(args, domain)
	}
	if !cutoff.IsZero() {
		conds = append(conds, "m.started_at >= ?")
		args = append(args, cutoff.Format(time.RFC3339))
	}
	where := ""
	if len(conds) > 0 {
		where = "WHERE " + strings.Join(conds, " AND ")
	}

	// Single JOIN query: aggregate phases per mission
	query := fmt.Sprintf(`
		SELECT
			m.mission_id,
			COUNT(p.id)                                                    AS total_phases,
			SUM(CASE WHEN p.status IN ('completed','running') THEN 1 ELSE 0 END) AS completed_phases,
			SUM(CASE WHEN p.status = 'failed'                THEN 1 ELSE 0 END) AS failed_phases,
			SUM(COALESCE(p.duration_seconds, 0))                           AS total_dur_s
		FROM mission_metrics m
		LEFT JOIN phase_metrics p ON p.mission_id = m.mission_id
		%s
		GROUP BY m.mission_id`, where)

	rows, err := db.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("query v1 stats: %w", err)
	}
	defer rows.Close()

	s := &stats{}
	for rows.Next() {
		var mID string
		var totalPhases, completedPhases, failedPhases int
		var totalDurS float64
		if err := rows.Scan(&mID, &totalPhases, &completedPhases, &failedPhases, &totalDurS); err != nil {
			continue
		}

		s.Missions++
		s.Phases += totalPhases
		s.PhasesCompleted += completedPhases
		s.PhasesFailed += failedPhases
		s.TotalDurS += totalDurS

		if failedPhases == 0 {
			s.Successes++
		} else if completedPhases == 0 {
			s.Failures++
		} else {
			s.Partials++
		}
	}

	// Load individual phase durations for avg calculation
	pdQuery := fmt.Sprintf(`
		SELECT p.duration_seconds
		FROM phase_metrics p
		JOIN mission_metrics m ON m.mission_id = p.mission_id
		WHERE p.duration_seconds IS NOT NULL AND p.duration_seconds > 0
		%s`, func() string {
		if where != "" {
			return "AND " + strings.TrimPrefix(where, "WHERE ")
		}
		return ""
	}())
	pdrows, err := db.Query(pdQuery, args...)
	if err == nil {
		defer pdrows.Close()
		for pdrows.Next() {
			var dur float64
			if pdrows.Scan(&dur) == nil {
				s.PhaseDurations = append(s.PhaseDurations, dur)
			}
		}
	}

	return s, nil
}

func loadV2Stats(domain string, cutoff time.Time) (*stats, error) {
	base, err := config.Dir()
	if err != nil {
		return nil, fmt.Errorf("config dir: %w", err)
	}
	path := filepath.Join(base, "metrics.jsonl")

	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return &stats{}, nil
		}
		return nil, fmt.Errorf("open metrics: %w", err)
	}
	defer f.Close()

	s := &stats{}
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 4*1024*1024), 4*1024*1024) // 4MB buffer for phase arrays
	for scanner.Scan() {
		var e v2Entry
		if json.Unmarshal(scanner.Bytes(), &e) != nil {
			continue
		}
		if domain != "" && e.Domain != domain {
			continue
		}
		if !cutoff.IsZero() && e.StartedAt.Before(cutoff) {
			continue
		}

		s.Missions++
		switch e.Status {
		case "success":
			s.Successes++
		case "failure":
			s.Failures++
		default:
			s.Partials++
		}
		s.TotalDurS += float64(e.DurationSec)
		s.Phases += e.PhasesTotal
		s.PhasesCompleted += e.PhasesCompleted
		s.PhasesFailed += e.PhasesFailed
		s.LearningsRetrieved += e.LearningsRetrieved
		s.RetriesTotal += e.RetriesTotal
		s.GateFailures += e.GateFailures
		s.OutputLenTotal += e.OutputLenTotal

		for _, p := range e.Phases {
			s.PhaseDurations = append(s.PhaseDurations, float64(p.DurationS))
		}
	}

	return s, nil
}

// ── formatting helpers ────────────────────────────────────────────────────────

func delta(v1val, v2val float64, higherIsBetter bool) string {
	if v1val == 0 {
		return "   —  "
	}
	pct := (v2val - v1val) / v1val * 100
	if math.Abs(pct) < 1 {
		return "  ~0% "
	}
	better := (pct > 0) == higherIsBetter
	sign := "▲"
	if pct < 0 {
		sign = "▼"
	}
	indicator := "✓"
	if !better {
		indicator = "✗"
	}
	return fmt.Sprintf("%s%s %.1f%%", indicator, sign, math.Abs(pct))
}

func fmtF(v float64, unit string) string {
	if v == 0 {
		return "—"
	}
	return fmt.Sprintf("%.1f%s", v, unit)
}

func fmtI(v int) string {
	if v == 0 {
		return "—"
	}
	return fmt.Sprintf("%d", v)
}

func printReport(v1, v2 *stats, domain string, days int) {
	sep := strings.Repeat("─", 72)

	// Header
	fmt.Println()
	fmt.Printf("  %-72s\n", "ORCHESTRATOR v1 vs v2 — PERFORMANCE COMPARISON")
	subtitle := "all time"
	if days > 0 {
		subtitle = fmt.Sprintf("last %d days", days)
	}
	if domain != "" {
		subtitle += " • " + domain
	}
	fmt.Printf("  %s\n", subtitle)
	fmt.Println()

	fmt.Printf("  %-30s  %-18s  %-18s  %s\n", "METRIC", "v1", "v2", "DELTA")
	fmt.Println("  " + sep)

	// SPEED
	fmt.Println("  SPEED")
	printRow("  Avg mission duration", fmtF(v1.avgMissionDurS(), "s"), fmtF(v2.avgMissionDurS(), "s"),
		delta(v1.avgMissionDurS(), v2.avgMissionDurS(), false))
	printRow("  Avg phase duration", fmtF(v1.avgPhaseDurS(), "s"), fmtF(v2.avgPhaseDurS(), "s"),
		delta(v1.avgPhaseDurS(), v2.avgPhaseDurS(), false))
	fmt.Println()

	// RELIABILITY
	fmt.Println("  RELIABILITY")
	printRow("  Mission success rate", fmtF(v1.successRate(), "%"), fmtF(v2.successRate(), "%"),
		delta(v1.successRate(), v2.successRate(), true))
	printRow("  Phase success rate", fmtF(v1.phaseSuccessRate(), "%"), fmtF(v2.phaseSuccessRate(), "%"),
		delta(v1.phaseSuccessRate(), v2.phaseSuccessRate(), true))
	printRow("  Retries/mission (v2 new)", "—", fmtF(v2.avgRetriesPerMission(), ""), "")
	printRow("  Gate failures/mission (v2 new)", "—", fmtF(v2.avgGateFailuresPerMission(), ""), "")
	fmt.Println()

	// EFFICIENCY
	fmt.Println("  EFFICIENCY")
	printRow("  Learnings retrieved/mission", "—", fmtF(v2.avgLearningsRetrieved(), ""), "")
	printRow("  Avg output/phase (chars)", "—", fmtF(v2.avgOutputLenPerPhase(), ""), "")
	printRow("  Avg phases/mission",
		fmtF(func() float64 {
			if v1.Missions == 0 {
				return 0
			}
			return float64(v1.Phases) / float64(v1.Missions)
		}(), ""),
		fmtF(func() float64 {
			if v2.Missions == 0 {
				return 0
			}
			return float64(v2.Phases) / float64(v2.Missions)
		}(), ""), "")
	fmt.Println()

	// VOLUME
	fmt.Println("  VOLUME")
	printRow("  Total missions", fmtI(v1.Missions), fmtI(v2.Missions), "")
	printRow("  Total phases", fmtI(v1.Phases), fmtI(v2.Phases), "")
	printRow("  Successes / failures",
		fmt.Sprintf("%d / %d", v1.Successes, v1.Failures),
		fmt.Sprintf("%d / %d", v2.Successes, v2.Failures), "")
	fmt.Println()

	// Summary note
	if v2.Missions < 5 {
		fmt.Printf("  ⚠  v2 sample is small (%d missions). Run more benchmarks for confidence.\n\n", v2.Missions)
	}
}

func printRow(label, v1val, v2val, d string) {
	fmt.Printf("  %-30s  %-18s  %-18s  %s\n", label, v1val, v2val, d)
}
