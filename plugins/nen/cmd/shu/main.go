// shu — Nanika self-evaluation tool
//
// Usage:
//
//	shu evaluate                         Evaluate all nanika components and save findings
//	shu propose [--dry-run] [--json]     Propose remediation missions from findings
//	shu propose --init                   Create scheduler jobs (propose every 4h, dispatch every 15m)
//	shu query status  [--json]
//	shu query items   [--json]
//	shu query actions [--json]
package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
	_ "modernc.org/sqlite"
)

const (
	criticalThreshold = 50
	findingsFileName  = "shu-findings.json"
)

// ComponentResult holds the result of a single component evaluation.
type ComponentResult struct {
	Name   string   `json:"name"`
	Score  int      `json:"score"`
	Trend  string   `json:"trend"`
	Issues []string `json:"issues,omitempty"`
}

// shuEvaluation is the persisted evaluation state.
type shuEvaluation struct {
	EvaluatedAt time.Time         `json:"evaluated_at"`
	Results     []ComponentResult `json:"results"`
}

// pluginStatus is used for unmarshaling plugin query status output.
type pluginStatus struct {
	Status string `json:"status"`
}

// koRunSummary is the run-level summary from `ko results --json`.
type koRunSummary struct {
	ID          string    `json:"ID"`
	ConfigPath  string    `json:"ConfigPath"`
	Description string    `json:"Description"`
	StartedAt   time.Time `json:"StartedAt"`
	Total       int       `json:"Total"`
	Passed      int       `json:"Passed"`
	Failed      int       `json:"Failed"`
}

// koResultsOutput is the top-level JSON from `ko results --json`.
type koResultsOutput struct {
	Run koRunSummary `json:"run"`
}

// FindingSummary is a condensed finding for query output.
type FindingSummary struct {
	ID       string    `json:"id"`
	Ability  string    `json:"ability"`
	Severity string    `json:"severity"`
	Category string    `json:"category"`
	Title    string    `json:"title"`
	FoundAt  time.Time `json:"found_at"`
}

// findingsDBPath returns the canonical path to the nen findings database.
func findingsDBPath() string {
	dir, err := scan.Dir()
	if err != nil {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, ".alluka", "nen", "findings.db")
	}
	return filepath.Join(dir, "nen", "findings.db")
}

// nenDaemonPIDPath returns the path to the nen-daemon PID file.
func nenDaemonPIDPath() string {
	dir, err := scan.Dir()
	if err != nil {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, ".alluka", "nen-daemon.pid")
	}
	return filepath.Join(dir, "nen-daemon.pid")
}

// isDaemonRunning checks whether nen-daemon is alive via its PID file.
func isDaemonRunning() bool {
	data, err := os.ReadFile(nenDaemonPIDPath())
	if err != nil {
		return false
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil || pid <= 0 {
		return false
	}
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	return proc.Signal(syscall.Signal(0)) == nil
}

// queryFindings returns up to limit active findings from findings.db.
// Returns nil, nil if the database does not yet exist.
func queryFindings(ctx context.Context, limit int) ([]FindingSummary, int, error) {
	path := findingsDBPath()
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return nil, 0, nil
	}
	db, err := sql.Open("sqlite", "file:"+path+"?mode=ro")
	if err != nil {
		return nil, 0, fmt.Errorf("open findings.db: %w", err)
	}
	defer db.Close()

	now := time.Now().UTC().Format(time.RFC3339)

	var total int
	if err := db.QueryRowContext(ctx, `
		SELECT COUNT(*) FROM findings
		WHERE superseded_by = ''
		  AND (expires_at IS NULL OR expires_at > ?)`, now,
	).Scan(&total); err != nil {
		return nil, 0, fmt.Errorf("count findings: %w", err)
	}

	rows, err := db.QueryContext(ctx, `
		SELECT id, ability, severity, category, title, found_at
		FROM findings
		WHERE superseded_by = ''
		  AND (expires_at IS NULL OR expires_at > ?)
		ORDER BY
			CASE severity
				WHEN 'critical' THEN 1
				WHEN 'high'     THEN 2
				WHEN 'medium'   THEN 3
				WHEN 'low'      THEN 4
				ELSE                 5
			END,
			found_at DESC
		LIMIT ?`, now, limit,
	)
	if err != nil {
		return nil, total, fmt.Errorf("query findings: %w", err)
	}
	defer rows.Close()

	var findings []FindingSummary
	for rows.Next() {
		var f FindingSummary
		var foundAtStr string
		if err := rows.Scan(&f.ID, &f.Ability, &f.Severity, &f.Category, &f.Title, &foundAtStr); err != nil {
			continue
		}
		if t, err := time.Parse(time.RFC3339, foundAtStr); err == nil {
			f.FoundAt = t
		}
		findings = append(findings, f)
	}
	return findings, total, rows.Err()
}

func main() {
	flag.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: shu <command> [flags]\n\nCommands:\n  evaluate      Evaluate nanika component health\n  propose       Propose remediation missions from findings\n  propose --init  Create scheduler jobs (propose every 4h, dispatch every 15m, evaluate weekly)\n  dispatch      Dispatch the next approved mission (throttle-aware)\n  close         Close a tracker issue and mark findings as superseded\n  close --sweep Sweep remediation missions and reconcile resolved tracker issues\n  review        Interactively approve/reject pending proposals\n  review --approve <ID>  Approve a proposal (open → in-progress)\n  query         Query latest evaluation results (status|items|actions)\n")
	}

	args := os.Args[1:]
	if len(args) == 0 {
		flag.Usage()
		os.Exit(1)
	}

	switch args[0] {
	case "evaluate":
		if err := runEvaluate(args[1:]); err != nil {
			fmt.Fprintf(os.Stderr, "shu evaluate: %v\n", err)
			os.Exit(1)
		}
	case "propose":
		if err := runPropose(args[1:]); err != nil {
			fmt.Fprintf(os.Stderr, "shu propose: %v\n", err)
			os.Exit(1)
		}
	case "dispatch":
		if err := runDispatch(args[1:]); err != nil {
			fmt.Fprintf(os.Stderr, "shu dispatch: %v\n", err)
			os.Exit(1)
		}
	case "close":
		if err := runClose(args[1:]); err != nil {
			fmt.Fprintf(os.Stderr, "shu close: %v\n", err)
			os.Exit(1)
		}
	case "review":
		if err := runReview(args[1:]); err != nil {
			fmt.Fprintf(os.Stderr, "shu review: %v\n", err)
			os.Exit(1)
		}
	case "query":
		if err := runQuery(args[1:]); err != nil {
			fmt.Fprintf(os.Stderr, "shu query: %v\n", err)
			os.Exit(1)
		}
	default:
		fmt.Fprintf(os.Stderr, "shu: unknown command %q\n\n", args[0])
		flag.Usage()
		os.Exit(1)
	}
}

func runEvaluate(args []string) error {
	fs := flag.NewFlagSet("evaluate", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	jsonOutput := fs.Bool("json", false, "Output JSON array of component results")
	if err := fs.Parse(args); err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	prevState := loadState()

	type namedEval struct {
		name string
		fn   func(context.Context) ComponentResult
	}

	evaluators := []namedEval{
		{"engage", evaluateEngage},
		{"scout", evaluateScout},
		{"scheduler", evaluateScheduler},
		{"gmail", evaluateGmail},
		{"obsidian", evaluateObsidian},
		{"ynab", evaluateYnab},
		{"linkedin", evaluateLinkedIn},
		{"reddit", evaluateReddit},
		{"substack", evaluateSubstack},
		{"youtube", evaluateYouTube},
		{"elevenlabs", evaluateElevenLabs},
		{"ko", evaluateKo},
	}

	results := make([]ComponentResult, 0, len(evaluators))
	for _, e := range evaluators {
		cr := e.fn(ctx)
		cr.Name = e.name
		prev, ok := prevState[e.name]
		if !ok {
			cr.Trend = "new"
		} else if cr.Score > prev {
			cr.Trend = "up"
		} else if cr.Score < prev {
			cr.Trend = "down"
		} else {
			cr.Trend = "flat"
		}
		results = append(results, cr)
	}

	ev := shuEvaluation{
		EvaluatedAt: time.Now(),
		Results:     results,
	}
	saveFindings(ev)

	if *jsonOutput {
		return encodeJSON(results)
	}
	printComponentTable(results)
	return nil
}

// loadState returns the previous scores for trend computation.
func loadState() map[string]int {
	ev, err := loadFindings()
	if err != nil {
		return map[string]int{}
	}
	state := make(map[string]int, len(ev.Results))
	for _, r := range ev.Results {
		state[r.Name] = r.Score
	}
	return state
}

// runCommand executes a binary with args and returns combined stdout output.
func runCommand(ctx context.Context, binary string, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, binary, args...)
	out, err := cmd.Output()
	if err != nil {
		return nil, err
	}
	return out, nil
}

// binaryPath returns the path to a named binary in PATH.
func binaryPath(name string) string {
	p, err := exec.LookPath(name)
	if err != nil {
		return name
	}
	return p
}

// evaluatePlugin runs `<binary> query status --json` and returns a score based on the result.
func evaluatePlugin(ctx context.Context, name string) ComponentResult {
	cr := ComponentResult{Score: 100}

	bin := binaryPath(name)
	out, err := runCommand(ctx, bin, "query", "status", "--json")
	if err != nil {
		cr.Score = 0
		cr.Issues = []string{fmt.Sprintf("query status failed: %v", err)}
		return cr
	}
	var status map[string]any
	if err := json.Unmarshal(out, &status); err != nil {
		cr.Score = 50
		cr.Issues = []string{fmt.Sprintf("parsing status JSON: %v", err)}
		return cr
	}
	return cr
}

func evaluateGmail(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "gmail")
}

func evaluateObsidian(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "obsidian")
}

func evaluateYnab(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "ynab")
}

func evaluateLinkedIn(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "linkedin")
}

func evaluateReddit(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "reddit")
}

func evaluateSubstack(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "substack")
}

func evaluateYouTube(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "youtube")
}

func evaluateElevenLabs(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "elevenlabs")
}

func evaluateScout(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "scout")
}

func evaluateScheduler(ctx context.Context) ComponentResult {
	return evaluatePlugin(ctx, "scheduler")
}

// evaluateEngage checks engage queue stats via `engage query status --json`.
func evaluateEngage(ctx context.Context) ComponentResult {
	cr := ComponentResult{Score: 100}

	binaryPath := binaryPath("engage")
	out, err := runCommand(ctx, binaryPath, "query", "status", "--json")
	if err != nil {
		cr.Score = 30
		cr.Issues = []string{fmt.Sprintf("engage query status failed: %v", err)}
		return cr
	}

	var status struct {
		Pending  int `json:"pending"`
		Approved int `json:"approved"`
		Rejected int `json:"rejected"`
		Posted   int `json:"posted"`
	}
	if err := json.Unmarshal(out, &status); err != nil {
		cr.Score = 50
		cr.Issues = []string{fmt.Sprintf("parsing engage status JSON: %v", err)}
		return cr
	}

	var issues []string
	if status.Pending > 20 {
		issues = append(issues, fmt.Sprintf("%d items stuck in pending", status.Pending))
	} else if status.Pending > 10 {
		issues = append(issues, fmt.Sprintf("%d pending items in queue", status.Pending))
	}

	total := status.Pending + status.Approved + status.Posted + status.Rejected
	if total > 0 && status.Posted == 0 {
		issues = append(issues, "no items posted yet")
	}

	if len(issues) > 0 {
		cr.Score = 70
		cr.Issues = issues
	}
	return cr
}

// evaluateKo reads the most recent ko eval run via `ko results --json` and
// derives a health score from the pass rate. Running a new eval is expensive
// (claude-opus-4-6 + extended thinking); the scheduler job handles periodic
// re-evaluation. This function reports on what the last run produced.
// Runs below 80% are flagged [MEDIUM]; below 60% are flagged [HIGH].
func evaluateKo(ctx context.Context) ComponentResult {
	cr := ComponentResult{Score: 100}

	bin := binaryPath("ko")
	out, err := runCommand(ctx, bin, "results", "--json")
	if err != nil {
		cr.Score = 0
		cr.Issues = []string{fmt.Sprintf("ko results failed: %v", err)}
		return cr
	}

	var koOut koResultsOutput
	if err := json.Unmarshal(out, &koOut); err != nil {
		cr.Score = 50
		cr.Issues = []string{fmt.Sprintf("parsing ko results JSON: %v", err)}
		return cr
	}

	run := koOut.Run
	if run.Total == 0 {
		cr.Score = 50
		cr.Issues = []string{"ko results returned no tests — run `ko evaluate <config.yaml>` first"}
		return cr
	}

	passRate := float64(run.Passed) / float64(run.Total) * 100
	cr.Score = int(passRate)

	if passRate < 60 {
		cr.Issues = []string{fmt.Sprintf("[HIGH] %s: %d/%d passing (%.0f%%)", run.Description, run.Passed, run.Total, passRate)}
	} else if passRate < 80 {
		cr.Issues = []string{fmt.Sprintf("[MEDIUM] %s: %d/%d passing (%.0f%%)", run.Description, run.Passed, run.Total, passRate)}
	}

	return cr
}

// runQuery dispatches shu query <subcommand> [--json].
func runQuery(args []string) error {
	if len(args) == 0 {
		fmt.Fprintf(os.Stderr, "Usage: shu query <subcommand> [--json]\n\nSubcommands:\n  status    Aggregated health score and critical component count\n  items     Component results from the latest evaluation\n  actions   Available actions (none — read-only)\n")
		return fmt.Errorf("subcommand required")
	}
	switch args[0] {
	case "status":
		return runQueryStatus(args[1:])
	case "items":
		return runQueryItems(args[1:])
	case "actions":
		return runQueryActions(args[1:])
	default:
		return fmt.Errorf("unknown subcommand %q", args[0])
	}
}

func runQueryStatus(args []string) error {
	fs := flag.NewFlagSet("query status", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	jsonOutput := fs.Bool("json", false, "Output JSON")
	if err := fs.Parse(args); err != nil {
		return err
	}

	ev, err := loadFindings()
	if err != nil {
		if *jsonOutput {
			return encodeJSON(struct {
				Status string `json:"status"`
				Error  string `json:"error"`
			}{"degraded", err.Error()})
		}
		return fmt.Errorf("loading evaluation: %w", err)
	}

	var total, criticalCount int
	for _, r := range ev.Results {
		total += r.Score
		if r.Score < criticalThreshold {
			criticalCount++
		}
	}
	var score int
	if len(ev.Results) > 0 {
		score = total / len(ev.Results)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	daemonRunning := isDaemonRunning()
	_, activeFindings, _ := queryFindings(ctx, 0)

	if *jsonOutput {
		return encodeJSON(struct {
			Score          int       `json:"score"`
			CriticalCount  int       `json:"critical_count"`
			EvaluatedAt    time.Time `json:"evaluated_at"`
			DaemonRunning  bool      `json:"daemon_running"`
			ActiveFindings int       `json:"active_findings"`
		}{score, criticalCount, ev.EvaluatedAt, daemonRunning, activeFindings})
	}

	daemonStr := "stopped"
	if daemonRunning {
		daemonStr = "running"
	}
	fmt.Printf("Score: %d  Critical: %d  Evaluated: %s  Daemon: %s  Findings: %d\n",
		score, criticalCount, ev.EvaluatedAt.Format(time.RFC3339), daemonStr, activeFindings)
	return nil
}

func runQueryItems(args []string) error {
	fs := flag.NewFlagSet("query items", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	jsonOutput := fs.Bool("json", false, "Output JSON")
	if err := fs.Parse(args); err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	ev, err := loadFindings()
	if err != nil {
		if *jsonOutput {
			return encodeJSON(struct {
				Items    []ComponentResult `json:"items"`
				Count    int               `json:"count"`
				Findings []FindingSummary  `json:"findings"`
			}{[]ComponentResult{}, 0, []FindingSummary{}})
		}
		return fmt.Errorf("loading evaluation: %w", err)
	}

	nenFindings, _, _ := queryFindings(ctx, 20)
	if nenFindings == nil {
		nenFindings = []FindingSummary{}
	}

	if *jsonOutput {
		items := ev.Results
		if items == nil {
			items = []ComponentResult{}
		}
		return encodeJSON(struct {
			Items    []ComponentResult `json:"items"`
			Count    int               `json:"count"`
			Findings []FindingSummary  `json:"findings"`
		}{items, len(items), nenFindings})
	}
	printComponentTable(ev.Results)
	if len(nenFindings) > 0 {
		fmt.Printf("\nNen Findings (%d active):\n", len(nenFindings))
		fmt.Printf("%-40s  %-10s  %-20s  %s\n", "TITLE", "SEVERITY", "ABILITY", "FOUND AT")
		fmt.Println(strings.Repeat("-", 100))
		for _, f := range nenFindings {
			title := f.Title
			if len(title) > 38 {
				title = title[:35] + "..."
			}
			fmt.Printf("%-40s  %-10s  %-20s  %s\n", title, f.Severity, f.Ability, f.FoundAt.Format(time.RFC3339))
		}
	}
	return nil
}

func runQueryActions(args []string) error {
	fs := flag.NewFlagSet("query actions", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	jsonOutput := fs.Bool("json", false, "Output JSON")
	if err := fs.Parse(args); err != nil {
		return err
	}

	if *jsonOutput {
		return encodeJSON(struct {
			Actions []struct{} `json:"actions"`
		}{[]struct{}{}})
	}
	fmt.Println("No actions available (shu is read-only).")
	return nil
}

// findingsPath returns the path to the persisted evaluation findings file.
func findingsPath() string {
	dir, err := scan.Dir()
	if err != nil {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, ".alluka", findingsFileName)
	}
	return filepath.Join(dir, findingsFileName)
}

func saveFindings(ev shuEvaluation) {
	data, err := json.MarshalIndent(ev, "", "  ")
	if err != nil {
		return
	}
	_ = os.WriteFile(findingsPath(), data, 0o600)
}

func loadFindings() (shuEvaluation, error) {
	data, err := os.ReadFile(findingsPath())
	if err != nil {
		if os.IsNotExist(err) {
			return shuEvaluation{}, fmt.Errorf("no evaluation found — run 'shu evaluate' first")
		}
		return shuEvaluation{}, fmt.Errorf("reading findings: %w", err)
	}
	var ev shuEvaluation
	if err := json.Unmarshal(data, &ev); err != nil {
		return shuEvaluation{}, fmt.Errorf("parsing findings: %w", err)
	}
	return ev, nil
}

// encodeJSON writes v to stdout as indented JSON.
func encodeJSON(v any) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}

// printComponentTable writes results as a human-readable table.
func printComponentTable(results []ComponentResult) {
	fmt.Printf("%-15s  %5s  %-7s  %s\n", "COMPONENT", "SCORE", "TREND", "ISSUES")
	fmt.Println(strings.Repeat("-", 80))
	for _, r := range results {
		issues := strings.Join(r.Issues, "; ")
		if len(issues) > 60 {
			issues = issues[:57] + "..."
		}
		fmt.Printf("%-15s  %5d  %-7s  %s\n", r.Name, r.Score, r.Trend, issues)
	}
}
