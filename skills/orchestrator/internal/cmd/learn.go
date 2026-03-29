package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
)

func init() {
	learnCmd := &cobra.Command{
		Use:   "learn",
		Short: "Capture learnings from the most recent workspace",
		RunE:  runLearn,
	}

	statsCmd := &cobra.Command{
		Use:   "stats",
		Short: "Show learning database statistics",
		RunE:  showStats,
	}

	pruneCmd := &cobra.Command{
		Use:   "prune",
		Short: "Prune old and low-quality learnings from the database",
		Long:  "Removes learnings based on age, quality score, and domain count caps.\nDry-run by default — use --apply to actually delete.",
		RunE:  runPrune,
	}
	pruneCmd.Flags().Bool("apply", false, "actually delete (default is dry-run)")
	pruneCmd.Flags().Int("max-age", 180, "max age in days for unused low-quality learnings")
	pruneCmd.Flags().Float64("min-score", 0.1, "delete learnings below this quality score")
	pruneCmd.Flags().Int("max-count", 500, "max learnings per domain")

	rootCmd.AddCommand(learnCmd)
	rootCmd.AddCommand(statsCmd)
	rootCmd.AddCommand(pruneCmd)
}

func runLearn(cmd *cobra.Command, args []string) error {
	workspaces, err := core.ListWorkspaces()
	if err != nil {
		return err
	}
	if len(workspaces) == 0 {
		fmt.Println("no workspaces found")
		return nil
	}

	wsPath := workspaces[0] // most recent

	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	apiKey := learning.LoadAPIKey()
	embedder := learning.NewEmbedder(apiKey)

	ctx := context.Background()

	// Load checkpoint to get domain
	cp, _ := core.LoadCheckpoint(wsPath)
	dom := "dev"
	if cp != nil {
		dom = cp.Domain
	}

	// Scan worker output files
	learningsDir := core.LearningsDir(wsPath)
	workersDir := filepath.Join(wsPath, "workers")

	var totalCaptured int

	// Scan worker directories for output.md
	workers, _ := os.ReadDir(workersDir)
	for _, w := range workers {
		if !w.IsDir() {
			continue
		}

		outputPath := filepath.Join(workersDir, w.Name(), "output.md")
		data, err := os.ReadFile(outputPath)
		if err != nil {
			continue
		}

		wsID := filepath.Base(wsPath)
		text := string(data)

		// Marker-based capture
		captured := learning.CaptureFromText(text, w.Name(), dom, wsID)
		for _, l := range captured {
			if err := db.Insert(ctx, l, embedder); err != nil {
				if verbose {
					fmt.Printf("  warning: insert failed: %v\n", err)
				}
			} else {
				totalCaptured++
			}
		}

		// Persona-aware capture using focus areas
		// Worker names are "{persona}-{phase-id}" (e.g. "backend-engineer-phase-1"),
		// strip the last "-{segment}" suffix to resolve the persona key.
		personaName := workerToPersona(w.Name())
		if focusAreas := persona.GetLearningFocus(personaName); len(focusAreas) > 0 {
			focused := learning.CaptureWithFocus(ctx, text, focusAreas, personaName, dom, wsID)
			for _, l := range focused {
				if err := db.Insert(ctx, l, embedder); err != nil {
					if verbose {
						fmt.Printf("  warning: focus insert failed: %v\n", err)
					}
				} else {
					totalCaptured++
				}
			}
		}
	}

	// Parse learnings JSON files written by hook scripts
	jsonFiles, _ := os.ReadDir(learningsDir)
	for _, f := range jsonFiles {
		if f.IsDir() || !strings.HasSuffix(f.Name(), ".json") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(learningsDir, f.Name()))
		if err != nil {
			continue
		}
		hookLearnings := parseHookJSON(data, dom, filepath.Base(wsPath))
		for _, l := range hookLearnings {
			if err := db.Insert(ctx, l, embedder); err != nil {
				if verbose {
					fmt.Printf("  warning: hook insert failed: %v\n", err)
				}
			} else {
				totalCaptured++
			}
		}
	}

	fmt.Printf("captured %d learnings from %s\n", totalCaptured, filepath.Base(wsPath))
	return nil
}

func showStats(cmd *cobra.Command, args []string) error {
	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	total, withEmb, err := db.Stats()
	if err != nil {
		return err
	}

	totalC, injected, avgRate, err := db.ComplianceStats()
	if err != nil {
		return fmt.Errorf("compliance stats: %w", err)
	}
	_ = totalC // same as total from Stats()

	fmt.Printf("learnings: %d total, %d with embeddings\n", total, withEmb)

	if injected > 0 {
		fmt.Printf("compliance:  %d injected, avg rate %.0f%%\n", injected, avgRate*100)
	} else {
		fmt.Printf("compliance:  no injections recorded yet\n")
	}
	return nil
}

// workerToPersona strips the phase suffix from a worker name to get the persona key.
// e.g. "backend-engineer-phase-1" → "backend-engineer"
//      "researcher-phase-2"       → "researcher"
func workerToPersona(workerName string) string {
	// Worker names follow "{persona}-phase-{n}" convention
	const phaseSep = "-phase-"
	if idx := strings.LastIndex(workerName, phaseSep); idx > 0 {
		return workerName[:idx]
	}
	return workerName
}

// hookEntry is the JSON structure written by the learning capture hook script.
type hookEntry struct {
	Marker    string `json:"marker"`
	Type      string `json:"type"`
	Content   string `json:"content"`
	Timestamp string `json:"timestamp"`
}

// parseHookJSON parses newline-delimited JSON entries from hook output files.
// Each line is a JSON object with marker, type, content, and timestamp fields.
func parseHookJSON(data []byte, domain, workspaceID string) []learning.Learning {
	var result []learning.Learning

	for _, line := range strings.Split(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}

		var entry hookEntry
		if err := json.Unmarshal([]byte(line), &entry); err != nil {
			continue
		}

		content := strings.TrimSpace(entry.Content)
		if len(content) < 20 {
			continue
		}

		ltype := learning.LearningType(entry.Type)
		if ltype == "" {
			ltype = learning.TypeInsight
		}

		ts := time.Now()
		if entry.Timestamp != "" {
			if parsed, err := time.Parse(time.RFC3339, entry.Timestamp); err == nil {
				ts = parsed
			}
		}

		result = append(result, learning.Learning{
			ID:          fmt.Sprintf("hook_%d", time.Now().UnixNano()),
			Type:        ltype,
			Content:     content,
			Domain:      domain,
			WorkspaceID: workspaceID,
			CreatedAt:   ts,
		})
	}
	return result
}

func runPrune(cmd *cobra.Command, args []string) error {
	apply, _ := cmd.Flags().GetBool("apply")
	maxAge, _ := cmd.Flags().GetInt("max-age")
	minScore, _ := cmd.Flags().GetFloat64("min-score")
	maxCount, _ := cmd.Flags().GetInt("max-count")

	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	opts := learning.CleanupOptions{
		MaxAgeDays:   maxAge,
		MinScore:     minScore,
		MaxPerDomain: maxCount,
		DryRun:       !apply,
	}

	n, err := db.Cleanup(context.Background(), opts)
	if err != nil {
		return fmt.Errorf("cleanup: %w", err)
	}

	if opts.DryRun {
		fmt.Printf("dry-run: would remove %d learnings (use --apply to delete)\n", n)
	} else {
		fmt.Printf("removed %d learnings\n", n)
	}
	return nil
}
