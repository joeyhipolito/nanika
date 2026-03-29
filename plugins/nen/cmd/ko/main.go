package main

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nen/internal/audit"
	"github.com/joeyhipolito/nen/ko"
)

const defaultModel = "claude-opus-4-6"

func main() {
	if len(os.Args) < 2 {
		printUsage()
		os.Exit(1)
	}

	ctx := context.Background()
	cmd := os.Args[1]

	var err error
	switch cmd {
	case "evaluate":
		err = cmdEvaluate(ctx, os.Args[2:])
	case "results":
		err = cmdResults(ctx, os.Args[2:])
	case "history":
		err = cmdHistory(ctx, os.Args[2:])
	case "improve":
		err = cmdImprove(ctx, os.Args[2:])
	case "apply":
		err = cmdApply(ctx, os.Args[2:])
	default:
		fmt.Fprintf(os.Stderr, "unknown command: %s\n\n", cmd)
		printUsage()
		os.Exit(1)
	}

	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}

func printUsage() {
	fmt.Fprint(os.Stderr, `ko — LLM eval engine + audit apply

Usage:
  ko evaluate <config.yaml>  [--model MODEL] [--concurrency N] [--db PATH]
                             [--cache] [--no-cache] [--cache-ttl DURATION] [--cache-db PATH]
                             [--dual-judge] [--use-cli]
  ko results  [config.yaml]  [--run N] [--failures] [--json] [--db PATH]
  ko history                 [--limit N] [--config PATH] [--db PATH]
  ko improve  <config.yaml>  [--model MODEL] [--db PATH]
  ko apply    <workspace-id> [--dry-run] [--model MODEL] [--format text|json]

Commands:
  evaluate    Run evals from a YAML config and store results in history
  results     Show per-test results for a past run (table or JSON)
  history     Show past eval runs
  improve     Analyze last run's failures and suggest improvements via Claude
  apply       Apply audit recommendations to persona files and SKILL.md

Flags:
  --use-cli   Use claude CLI instead of Anthropic API (defaults to claude-haiku-4-5 for cost efficiency)
`)
}

// ── Audit apply subcommand ───────────────────────────────────────────────────

func cmdApply(ctx context.Context, args []string) error {
	fs := flag.NewFlagSet("apply", flag.ContinueOnError)
	dryRun := fs.Bool("dry-run", false, "preview changes without applying")
	format := fs.String("format", "text", "output format: text, json")
	model := fs.String("model", "", "override LLM model (default: opus)")
	verbose := fs.Bool("verbose", false, "verbose output")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if fs.NArg() < 1 {
		return fmt.Errorf("usage: ko apply <workspace-id>")
	}

	wsID := fs.Arg(0)

	ctx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()

	result, err := audit.ApplyRecommendations(ctx, audit.ApplyOptions{
		ReportID: wsID,
		DryRun:   *dryRun,
		Model:    *model,
		Verbose:  *verbose,
		Confirm:  confirmApply,
	})
	if err != nil {
		return fmt.Errorf("apply failed: %w", err)
	}

	switch *format {
	case "json":
		out, fmtErr := audit.FormatApplyResultJSON(result)
		if fmtErr != nil {
			return fmtErr
		}
		fmt.Println(out)
	default:
		fmt.Print(audit.FormatApplyResult(result))
	}

	return nil
}

// confirmApply prompts the user for confirmation before applying changes.
func confirmApply(summary string) bool {
	fmt.Print("Apply these changes? [y/N] ")
	scanner := bufio.NewScanner(os.Stdin)
	if scanner.Scan() {
		answer := strings.TrimSpace(strings.ToLower(scanner.Text()))
		return answer == "y" || answer == "yes"
	}
	return false
}

// ── Eval subcommands (unchanged) ─────────────────────────────────────────────

// cmdEvaluate loads an eval config, runs all tests, persists each result
// immediately to the DB, and prints a summary.
func cmdEvaluate(ctx context.Context, args []string) error {
	fs := flag.NewFlagSet("evaluate", flag.ContinueOnError)
	model := fs.String("model", defaultModel, "LLM model to use")
	concurrency := fs.Int("concurrency", 1, "parallel test workers")
	dbPath := fs.String("db", ko.DefaultDBPath(), "path to ko-history.db")
	useCache := fs.Bool("cache", false, "enable response caching (keyed by model+prompt hash)")
	noCache := fs.Bool("no-cache", false, "disable response caching (overrides --cache)")
	cacheTTL := fs.Duration("cache-ttl", 24*time.Hour, "cache entry TTL (e.g. 1h, 24h, 7d)")
	cacheDB := fs.String("cache-db", ko.DefaultCacheDBPath(), "path to ko-cache.db")
	dualJudge := fs.Bool("dual-judge", false, "run a secondary Codex judge on all LLM assertions; disagreements flagged as REVIEW")
	useCli := fs.Bool("use-cli", false, "use claude CLI instead of Anthropic API")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if fs.NArg() < 1 {
		return fmt.Errorf("usage: ko evaluate <config.yaml>")
	}

	configPath, err := filepath.Abs(fs.Arg(0))
	if err != nil {
		return err
	}

	cfg, err := ko.LoadEvalConfig(ctx, configPath)
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}

	if *dualJudge {
		applyDualJudge(cfg)
	}

	// Use claude-haiku-4-5 as default for CLI mode (cheaper), opus for API mode
	modelToUse := *model
	if *useCli && modelToUse == defaultModel {
		modelToUse = "claude-haiku-4-5-20251001"
	}

	db, err := ko.OpenDB(*dbPath)
	if err != nil {
		return fmt.Errorf("open db: %w", err)
	}
	defer db.Close()

	// --no-cache takes precedence over --cache.
	cacheEnabled := *useCache && !*noCache

	var cache *ko.Cache
	if cacheEnabled {
		cache, err = ko.OpenCache(*cacheDB, *cacheTTL)
		if err != nil {
			return fmt.Errorf("open cache: %w", err)
		}
		defer cache.Close()
	}

	runID := newRunID()
	if err := db.CreateRun(ctx, runID, configPath, cfg.Description, modelToUse); err != nil {
		return fmt.Errorf("create run: %w", err)
	}

	fmt.Printf("Running eval: %s\n", cfg.Description)
	fmt.Printf("Config:  %s\n", configPath)
	fmt.Printf("Model:   %s\n", modelToUse)
	fmt.Printf("Run ID:  %s\n", runID)
	if *useCli {
		fmt.Printf("Mode:    CLI\n")
	}
	if cacheEnabled {
		fmt.Printf("Cache:   enabled (ttl=%s db=%s)\n", *cacheTTL, *cacheDB)
	}
	if *dualJudge {
		fmt.Printf("Judge:   dual (primary API + Codex)\n")
	}
	fmt.Println()

	total := len(cfg.Tests)
	counter := 0
	tracker := &ko.CostTracker{}

	opts := ko.RunnerOptions{
		Concurrency: *concurrency,
		Model:       modelToUse,
		UseCliMode:  *useCli,
		OnResult: func(r *ko.TestResult) {
			counter++
			status := "PASS"
			if !r.Passed {
				status = "FAIL"
			}
			line := fmt.Sprintf("[%*d/%d] %-50s %s (%dms)",
				len(fmt.Sprint(total)), counter, total,
				truncate(r.Description, 50), status, r.DurationMs)
			if r.CacheHit {
				line += " [cached]"
				tracker.RecordHit()
			} else {
				tracker.Record(ko.TokenUsage{
					InputTokens:  r.InputTokens,
					OutputTokens: r.OutputTokens,
				})
			}
			fmt.Println(line)
			if !r.Passed {
				for _, a := range r.Assertions {
					if !a.Passed {
						fmt.Printf("         - %s: %s\n", a.Type, a.Message)
					}
				}
			}
			if dbErr := db.InsertResult(ctx, runID, r); dbErr != nil {
				fmt.Fprintf(os.Stderr, "warn: persist result: %v\n", dbErr)
			}
		},
	}

	runner := ko.NewRunner(cfg, opts)
	results, err := runner.Run(ctx, func(ctx context.Context, prompt string) (string, error) {
		// Check cache first
		if cache != nil {
			key := ko.CacheKey(*model, prompt)
			if cached, ok, cErr := cache.Get(ctx, key); cErr == nil && ok {
				ko.RecordCacheHit(ctx)
				return cached, nil
			}
		}
		text, qErr := ko.CallLLM(ctx, modelToUse, prompt)
		if qErr != nil {
			return "", qErr
		}
		if cache != nil {
			key := ko.CacheKey(*model, prompt)
			if sErr := cache.Set(ctx, key, *model, text); sErr != nil {
				fmt.Fprintf(os.Stderr, "warn: cache set: %v\n", sErr)
			}
		}
		return text, nil
	})
	if err != nil {
		return fmt.Errorf("run eval: %w", err)
	}

	if dbErr := db.FinishRun(ctx, runID, results.Total, results.Passed, results.Failed,
		results.InputTokens, results.OutputTokens, results.CostUSD); dbErr != nil {
		fmt.Fprintf(os.Stderr, "warn: finish run: %v\n", dbErr)
	}

	fmt.Printf("\nResults: %d passed, %d failed (%d total)\n", results.Passed, results.Failed, results.Total)
	fmt.Printf("%s\n", tracker.FormatSummary(*model))
	if results.Failed > 0 {
		return fmt.Errorf("%d test(s) failed", results.Failed)
	}
	return nil
}

// cmdHistory lists recent eval runs from the database.
func cmdHistory(ctx context.Context, args []string) error {
	fs := flag.NewFlagSet("history", flag.ContinueOnError)
	limit := fs.Int("limit", 20, "max runs to show")
	configFilter := fs.String("config", "", "filter by config file path")
	dbPath := fs.String("db", ko.DefaultDBPath(), "path to ko-history.db")
	if err := fs.Parse(args); err != nil {
		return err
	}

	filter := *configFilter
	if fs.NArg() > 0 {
		abs, err := filepath.Abs(fs.Arg(0))
		if err != nil {
			return err
		}
		filter = abs
	}

	db, err := ko.OpenDB(*dbPath)
	if err != nil {
		return fmt.Errorf("open db: %w", err)
	}
	defer db.Close()

	runs, err := db.ListRuns(ctx, filter, *limit)
	if err != nil {
		return fmt.Errorf("list runs: %w", err)
	}
	if len(runs) == 0 {
		fmt.Println("No eval runs found.")
		return nil
	}

	fmt.Printf("%-20s  %-8s  %-6s  %s\n", "Started", "Result", "Model", "Description / Config")
	fmt.Println(strings.Repeat("─", 80))
	for _, r := range runs {
		ts := truncateTimestamp(r.StartedAt)
		result := fmt.Sprintf("%d/%d", r.Passed, r.Total)
		desc := r.Description
		if desc == "" {
			desc = filepath.Base(r.ConfigPath)
		}
		modelShort := r.Model
		if len(modelShort) > 6 {
			modelShort = modelShort[:6]
		}
		fmt.Printf("%-20s  %-8s  %-6s  %s\n", ts, result, modelShort, desc)
	}
	return nil
}

// cmdImprove loads the last eval run for a config, collects failed tests,
// and asks Claude to suggest improvements.
func cmdImprove(ctx context.Context, args []string) error {
	fs := flag.NewFlagSet("improve", flag.ContinueOnError)
	model := fs.String("model", defaultModel, "LLM model for analysis")
	dbPath := fs.String("db", ko.DefaultDBPath(), "path to ko-history.db")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if fs.NArg() < 1 {
		return fmt.Errorf("usage: ko improve <config.yaml>")
	}

	configPath, err := filepath.Abs(fs.Arg(0))
	if err != nil {
		return err
	}

	db, err := ko.OpenDB(*dbPath)
	if err != nil {
		return fmt.Errorf("open db: %w", err)
	}
	defer db.Close()

	run, err := db.GetLastRunForConfig(ctx, configPath)
	if err != nil {
		return fmt.Errorf("get last run: %w", err)
	}
	if run == nil {
		return fmt.Errorf("no history for %s — run `ko evaluate` first", configPath)
	}

	results, err := db.GetRunResults(ctx, run.ID)
	if err != nil {
		return fmt.Errorf("get results: %w", err)
	}

	var failures []ko.TestResult
	for _, r := range results {
		if !r.Passed {
			failures = append(failures, r)
		}
	}
	if len(failures) == 0 {
		fmt.Printf("All tests passed in the last run (%s). Nothing to improve.\n", run.ID)
		return nil
	}

	fmt.Printf("Analyzing %d failure(s) from run %s (%s)...\n\n", len(failures), run.ID, run.StartedAt)

	var sb strings.Builder
	sb.WriteString("You are a prompt engineering expert reviewing LLM eval failures.\n\n")
	sb.WriteString(fmt.Sprintf("Config: %s\n", configPath))
	sb.WriteString(fmt.Sprintf("Description: %s\n", run.Description))
	sb.WriteString(fmt.Sprintf("Last run: %d passed, %d failed, %d total\n\n", run.Passed, run.Failed, run.Total))
	sb.WriteString("Failed tests:\n\n")

	for i, f := range failures {
		sb.WriteString(fmt.Sprintf("=== Failure %d: %q ===\n", i+1, f.Description))
		if f.Error != "" {
			sb.WriteString(fmt.Sprintf("Error: %s\n", f.Error))
		} else {
			output := f.Output
			if len(output) > 600 {
				output = output[:600] + "... (truncated)"
			}
			sb.WriteString(fmt.Sprintf("LLM output:\n%s\n\n", output))
			sb.WriteString("Failed assertions:\n")
			for _, a := range f.Assertions {
				if !a.Passed {
					sb.WriteString(fmt.Sprintf("  type=%s  value=%q\n  message: %s\n", a.Type, a.Value, a.Message))
				}
			}
		}
		sb.WriteString("\n")
	}

	sb.WriteString("Please provide specific, actionable suggestions to fix these failures:\n")
	sb.WriteString("1. Changes to the prompt template to produce the expected output format or content\n")
	sb.WriteString("2. Adjustments to test assertions if expectations are unrealistic or too strict\n")
	sb.WriteString("3. Improvements to test variables if inputs are ambiguous\n\n")
	sb.WriteString("Quote the exact assertion values and show what you would change them to.\n")

	suggestion, err := ko.CallLLM(ctx, *model, sb.String())
	if err != nil {
		return fmt.Errorf("query claude: %w", err)
	}

	fmt.Println("─── Improvement suggestions ─────────────────────────────────────────────────")
	fmt.Println(suggestion)
	return nil
}

// cmdResults shows per-test results for a past eval run in a terminal table or JSON.
func cmdResults(ctx context.Context, args []string) error {
	fs := flag.NewFlagSet("results", flag.ContinueOnError)
	runN := fs.Int("run", 1, "run number to show (1 = most recent)")
	failuresOnly := fs.Bool("failures", false, "show only failed tests")
	jsonOutput := fs.Bool("json", false, "machine-readable JSON output")
	dbPath := fs.String("db", ko.DefaultDBPath(), "path to ko-history.db")
	if err := fs.Parse(args); err != nil {
		return err
	}

	// Optional positional: config path to restrict run selection.
	var configFilter string
	if fs.NArg() > 0 {
		abs, err := filepath.Abs(fs.Arg(0))
		if err != nil {
			return err
		}
		configFilter = abs
	}

	db, err := ko.OpenDB(*dbPath)
	if err != nil {
		return fmt.Errorf("open db: %w", err)
	}
	defer db.Close()

	runs, err := db.ListRuns(ctx, configFilter, *runN)
	if err != nil {
		return fmt.Errorf("list runs: %w", err)
	}
	if len(runs) < *runN {
		return fmt.Errorf("run %d not found (only %d run(s) in history)", *runN, len(runs))
	}
	run := runs[*runN-1]

	results, err := db.GetRunResults(ctx, run.ID)
	if err != nil {
		return fmt.Errorf("get results: %w", err)
	}

	if *failuresOnly {
		var filtered []ko.TestResult
		for _, r := range results {
			if !r.Passed {
				filtered = append(filtered, r)
			}
		}
		results = filtered
	}

	if *jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(struct {
			Run     ko.EvalRunSummary `json:"run"`
			Results []ko.TestResult   `json:"results"`
		}{Run: run, Results: results})
	}

	// ── Terminal table ────────────────────────────────────────────────────────
	fmt.Printf("Run:         %s  (%s)\n", run.ID, truncateTimestamp(run.StartedAt))
	fmt.Printf("Config:      %s\n", filepath.Base(run.ConfigPath))
	if run.Description != "" {
		fmt.Printf("Description: %s\n", run.Description)
	}
	fmt.Printf("Results:     %d/%d passed", run.Passed, run.Total)
	if run.CostUSD > 0 {
		fmt.Printf("  cost: $%.6f", run.CostUSD)
	}
	fmt.Println()
	fmt.Println()

	if len(results) == 0 {
		fmt.Println("No results to show.")
		return nil
	}

	const (
		colNum      = 4
		colStatus   = 6
		colScore    = 5
		colName     = 50
		colReasoning = 60
	)
	fmt.Printf("%-*s  %-*s  %-*s  %-*s  %s\n",
		colNum, "#",
		colStatus, "Status",
		colScore, "Score",
		colName, "Test Name",
		"Reasoning")
	fmt.Println(strings.Repeat("─", colNum+2+colStatus+2+colScore+2+colName+2+colReasoning))

	for i, r := range results {
		status := "PASS"
		if !r.Passed {
			status = "FAIL"
		}

		var passedAssert, totalAssert int
		reasoning := ""
		for _, a := range r.Assertions {
			totalAssert++
			if a.Passed {
				passedAssert++
			}
			if reasoning == "" && a.Reasoning != "" {
				reasoning = a.Reasoning
			}
		}
		score := "-"
		if totalAssert > 0 {
			score = fmt.Sprintf("%d/%d", passedAssert, totalAssert)
		}
		if reasoning == "" {
			for _, a := range r.Assertions {
				if !a.Passed && a.Message != "" {
					reasoning = a.Message
					break
				}
			}
		}

		fmt.Printf("%-*d  %-*s  %-*s  %-*s  %s\n",
			colNum, i+1,
			colStatus, status,
			colScore, score,
			colName, truncate(r.Description, colName),
			truncate(reasoning, colReasoning))
	}

	return nil
}

// applyDualJudge sets Dual=true on all LLM-backed assertions in cfg.
// This is equivalent to adding dual: true to every llm-rubric, similar,
// factuality, and answer-relevance assertion in the YAML.
func applyDualJudge(cfg *ko.EvalConfig) {
	for i := range cfg.DefaultTest.Assert {
		setDualRecursive(&cfg.DefaultTest.Assert[i])
	}
	for i := range cfg.Tests {
		for j := range cfg.Tests[i].Assert {
			setDualRecursive(&cfg.Tests[i].Assert[j])
		}
	}
}

func setDualRecursive(a *ko.AssertionConfig) {
	switch a.Type {
	case "llm-rubric", "similar", "factuality", "answer-relevance":
		a.Dual = true
	}
	for i := range a.Assert {
		setDualRecursive(&a.Assert[i])
	}
}

// ── Anthropic API ─────────────────────────────────────────────────────────────



// ── Helpers ───────────────────────────────────────────────────────────────────

// newRunID generates a unique run identifier using wall time + random bytes.
func newRunID() string {
	b := make([]byte, 6)
	_, _ = rand.Read(b)
	return fmt.Sprintf("%d-%x", time.Now().UnixNano(), b)
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n-1] + "…"
}

func truncateTimestamp(ts string) string {
	if len(ts) > 19 {
		return ts[:19]
	}
	return ts
}
