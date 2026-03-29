package cmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/metrics"
	"github.com/joeyhipolito/orchestrator-cli/internal/routing"
)

func init() {
	metricsCmd := &cobra.Command{
		Use:   "metrics",
		Short: "Show mission execution history",
		RunE:  showMetrics,
	}
	metricsCmd.Flags().Int("last", 20, "number of recent missions to show")
	metricsCmd.Flags().String("domain", "", "filter by domain")
	metricsCmd.Flags().String("status", "", "filter by status (success/failed/running)")
	metricsCmd.Flags().Int("days", 0, "show missions from last N days")
	metricsCmd.Flags().String("decomp-source", "", "filter by decomp source (predecomposed/decomp.llm/decomp.keyword/template/unknown)")

	metricsCmd.AddCommand(
		&cobra.Command{
			Use:   "personas",
			Short: "Per-persona phase count, duration, failure rate, and selection method breakdown",
			RunE:  showPersonaMetrics,
		},
		&cobra.Command{
			Use:   "skills",
			Short: "Skill usage frequency across all missions",
			RunE:  showSkillMetrics,
		},
		func() *cobra.Command {
			c := &cobra.Command{
				Use:   "trends",
				Short: "Daily mission count, success rate, and avg duration over time",
				RunE:  showTrends,
			}
			c.Flags().Int("days", 30, "number of days to include")
			return c
		}(),
		&cobra.Command{
			Use:   "routing",
			Short: "Routing success rates per persona from routing_decisions",
			RunE:  showRoutingMetrics,
		},
		&cobra.Command{
			Use:   "routing-methods",
			Short: "LLM vs keyword vs fallback routing distribution across all phases",
			RunE:  showRoutingMethodMetrics,
		},
		&cobra.Command{
			Use:   "phases <workspace-id>",
			Short: "Show phases and parsed skills for a mission",
			Args:  cobra.ExactArgs(1),
			RunE:  showPhaseMetrics,
		},
	)

	rootCmd.AddCommand(metricsCmd)
}

// openMetricsDB opens the SQLite metrics database and backfills only missing
// JSONL records so that read-only metrics commands never overwrite richer live
// SQLite phase or skill data.
func openMetricsDB(ctx context.Context) (*metrics.DB, error) {
	db, err := metrics.InitDB("")
	if err != nil {
		return nil, fmt.Errorf("opening metrics db: %w", err)
	}

	// Pass "" so ImportMissingFromJSONL resolves the JSONL path via config.Dir() itself.
	if _, err := db.ImportMissingFromJSONL(ctx, ""); err != nil {
		if ctx.Err() != nil {
			db.Close()
			return nil, fmt.Errorf("importing metrics jsonl: %w", err)
		}
		// Non-fatal for other errors (e.g. missing JSONL file, disk issues during import).
	}

	return db, nil
}

// truncate shortens s to at most n bytes, appending "..." if truncated.
func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n-3] + "..."
}

func showMetrics(cmd *cobra.Command, args []string) error {
	last, _ := cmd.Flags().GetInt("last")
	domainFilter, _ := cmd.Flags().GetString("domain")
	statusFilter, _ := cmd.Flags().GetString("status")
	days, _ := cmd.Flags().GetInt("days")
	decompSourceFilter, _ := cmd.Flags().GetString("decomp-source")

	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	db, err := openMetricsDB(ctx)
	if err != nil {
		return err
	}
	defer db.Close()

	missions, err := db.QueryMissions(ctx, last, domainFilter, days, statusFilter, decompSourceFilter)
	if err != nil {
		return fmt.Errorf("querying missions: %w", err)
	}
	if len(missions) == 0 {
		fmt.Println("no missions recorded yet")
		return nil
	}

	fmt.Printf("%-12s  %-10s  %-8s  %-14s  %-22s  %-8s %-12s  %s\n",
		"workspace", "domain", "status", "decomp", "persona", "duration", "phases", "task")
	fmt.Println(strings.Repeat("-", 114))

	var totalDur, successes, failures int
	for _, m := range missions {
		dur := fmt.Sprintf("%ds", m.DurationSec)
		phases := fmt.Sprintf("%d/%d", m.PhasesCompleted, m.PhasesTotal)
		if m.PhasesFailed > 0 {
			phases += fmt.Sprintf("(%df)", m.PhasesFailed)
		}
		task := truncate(strings.ReplaceAll(m.Task, "\n", " "), 50)
		persona := truncate(m.TopPersona, 22)
		wsID := truncate(m.WorkspaceID, 12)
		decomp := truncate(m.DecompSource, 14)
		fmt.Printf("%-12s  %-10s  %-8s  %-14s  %-22s  %-8s %-12s  %s\n",
			wsID, m.Domain, m.Status, decomp, persona, dur, phases, task)

		totalDur += m.DurationSec
		if m.Status == "success" {
			successes++
		} else if m.Status == "failed" {
			failures++
		}
	}

	avgDur := 0
	if len(missions) > 0 {
		avgDur = totalDur / len(missions)
	}
	fmt.Printf("\n%d missions  •  %d succeeded  •  %d failed  •  avg %ds\n",
		len(missions), successes, failures, avgDur)

	return nil
}

func showPersonaMetrics(cmd *cobra.Command, args []string) error {
	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	db, err := openMetricsDB(ctx)
	if err != nil {
		return err
	}
	defer db.Close()

	pms, err := db.QueryPersonaMetrics(ctx)
	if err != nil {
		return fmt.Errorf("querying persona metrics: %w", err)
	}
	if len(pms) == 0 {
		fmt.Println("no persona data recorded yet")
		return nil
	}

	fmt.Printf("%-30s  %6s  %8s  %6s  %9s  %5s  %5s\n",
		"persona", "phases", "avg_dur", "fail%", "avg_retry", "llm%", "kw%")
	fmt.Println(strings.Repeat("-", 80))

	for _, p := range pms {
		persona := truncate(p.Persona, 30)
		fmt.Printf("%-30s  %6d  %7.0fs  %5.1f%%  %9.2f  %4.0f%%  %4.0f%%\n",
			persona, p.PhaseCount, p.AvgDurationSec,
			p.FailureRate, p.AvgRetries, p.LLMPct, p.KeywordPct)
	}

	return nil
}

func showSkillMetrics(cmd *cobra.Command, args []string) error {
	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	db, err := openMetricsDB(ctx)
	if err != nil {
		return err
	}
	defer db.Close()

	skills, err := db.QuerySkillUsage(ctx)
	if err != nil {
		return fmt.Errorf("querying skill usage: %w", err)
	}
	if len(skills) == 0 {
		fmt.Println("no skill invocations recorded yet")
		return nil
	}

	fmt.Printf("%-28s  %-20s  %-24s  %-12s  %s\n", "skill", "phase", "persona", "source", "uses")
	fmt.Println(strings.Repeat("-", 96))

	for _, s := range skills {
		skill := truncate(s.SkillName, 28)
		phase := truncate(s.Phase, 20)
		persona := truncate(s.Persona, 24)
		source := truncate(s.Source, 12)
		fmt.Printf("%-28s  %-20s  %-24s  %-12s  %d\n", skill, phase, persona, source, s.Invocations)
	}

	return nil
}

func showTrends(cmd *cobra.Command, args []string) error {
	days, _ := cmd.Flags().GetInt("days")

	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	db, err := openMetricsDB(ctx)
	if err != nil {
		return err
	}
	defer db.Close()

	trends, err := db.QueryTrends(ctx, days)
	if err != nil {
		return fmt.Errorf("querying trends: %w", err)
	}
	if len(trends) == 0 {
		fmt.Printf("no missions in the last %d days\n", days)
		return nil
	}

	fmt.Printf("%-10s  %8s  %9s  %8s\n", "day", "missions", "success%", "avg_dur")
	fmt.Println(strings.Repeat("-", 42))

	var totalMissions, totalSuccess int
	var totalDur float64
	for _, t := range trends {
		successPct := 0.0
		if t.Total > 0 {
			successPct = float64(t.Successes) / float64(t.Total) * 100
		}
		fmt.Printf("%-10s  %8d  %8.1f%%  %7.0fs\n",
			t.Day, t.Total, successPct, t.AvgDuration)
		totalMissions += t.Total
		totalSuccess += t.Successes
		totalDur += t.AvgDuration * float64(t.Total)
	}
	overallPct := 0.0
	if totalMissions > 0 {
		overallPct = float64(totalSuccess) / float64(totalMissions) * 100
	}
	avgDur := 0.0
	if totalMissions > 0 {
		avgDur = totalDur / float64(totalMissions)
	}
	fmt.Printf("\n%d days  •  %d missions  •  %.1f%% success  •  avg %.0fs\n",
		len(trends), totalMissions, overallPct, avgDur)

	return nil
}

func showRoutingMetrics(cmd *cobra.Command, args []string) error {
	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	rdb, err := routing.OpenDB("")
	if err != nil {
		return fmt.Errorf("opening routing db: %w", err)
	}
	defer rdb.Close()

	summaries, err := rdb.GetPersonaRoutingStats(ctx)
	if err != nil {
		return fmt.Errorf("querying routing stats: %w", err)
	}
	if len(summaries) == 0 {
		fmt.Println("no routing decisions recorded yet")
		return nil
	}

	fmt.Printf("%-30s  %6s  %8s  %8s  %7s  %7s\n",
		"persona", "total", "success", "failure", "pending", "rate%")
	fmt.Println(strings.Repeat("-", 75))

	for _, s := range summaries {
		persona := truncate(s.Persona, 30)
		ratePct := s.SuccessRate * 100
		fmt.Printf("%-30s  %6d  %8d  %8d  %7d  %6.1f%%\n",
			persona, s.Total, s.Successes, s.Failures, s.Pending, ratePct)
		for _, f := range s.RecentFailures {
			age := int(time.Since(f.CreatedAt).Hours() / 24)
			reason := f.FailureReason
			if reason == "" {
				reason = "unknown"
			}
			phaseName := f.PhaseName
			if phaseName == "" {
				phaseName = f.PhaseID
			}
			fmt.Printf("  └ phase %q failed %d day(s) ago: %s\n",
				phaseName, age, truncate(reason, 60))
		}
	}

	return nil
}

func showRoutingMethodMetrics(cmd *cobra.Command, args []string) error {
	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	db, err := openMetricsDB(ctx)
	if err != nil {
		return err
	}
	defer db.Close()

	dist, err := db.QueryRoutingMethodDistribution(ctx)
	if err != nil {
		return fmt.Errorf("querying routing method distribution: %w", err)
	}
	if len(dist) == 0 {
		fmt.Println("no routing method data recorded yet")
		return nil
	}

	fmt.Printf("%-20s  %8s  %8s\n", "method", "phases", "pct%")
	fmt.Println(strings.Repeat("-", 42))

	var total int
	for _, r := range dist {
		fmt.Printf("%-20s  %8d  %7.1f%%\n", r.Method, r.Count, r.Pct)
		total += r.Count
	}
	fmt.Printf("\n%d phases total\n", total)

	if fbRate := metrics.FallbackRate(dist); fbRate > metrics.FallbackAlertThreshold {
		fmt.Printf("\nALERT: fallback routing rate %.1f%% exceeds %.0f%% threshold — "+
			"LLM decomposition may be failing too often\n", fbRate, metrics.FallbackAlertThreshold)
	}

	return nil
}

func showPhaseMetrics(cmd *cobra.Command, args []string) error {
	missionID := args[0]

	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	db, err := openMetricsDB(ctx)
	if err != nil {
		return err
	}
	defer db.Close()

	phases, err := db.QueryPhases(ctx, missionID)
	if err != nil {
		return fmt.Errorf("querying phases: %w", err)
	}
	if len(phases) == 0 {
		fmt.Printf("no phases recorded for mission %s\n", missionID)
		return nil
	}

	fmt.Printf("%-24s  %-28s  %-8s  %-8s  %s\n",
		"phase", "persona", "status", "duration", "parsed_skills")
	fmt.Println(strings.Repeat("-", 100))

	for _, p := range phases {
		skills := ""
		if len(p.ParsedSkills) > 0 {
			skills = strings.Join(p.ParsedSkills, ", ")
		}
		fmt.Printf("%-24s  %-28s  %-8s  %-7ds  %s\n",
			truncate(p.Name, 24), truncate(p.Persona, 28),
			p.Status, p.DurationS, skills)
	}

	return nil
}
