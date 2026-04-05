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
	"github.com/joeyhipolito/orchestrator-cli/internal/preflight"
)

func init() {
	hooksCmd := &cobra.Command{
		Use:   "hooks",
		Short: "Hook commands for context management during worker sessions",
	}

	flushCtxCmd := &cobra.Command{
		Use:   "flush-context",
		Short: "Write relevant learnings to a context bundle file",
		Long:  "Queries the learning database for relevant entries and writes them as a markdown bundle to the given output file.",
		RunE:  runFlushContext,
	}
	flushCtxCmd.Flags().String("query", "", "query describing the current task context (required)")
	flushCtxCmd.Flags().String("output", "", "path to write the context bundle (required)")
	flushCtxCmd.Flags().Int("limit", 10, "max learnings to include")
	flushCtxCmd.MarkFlagRequired("query")
	flushCtxCmd.MarkFlagRequired("output")

	injectCtxCmd := &cobra.Command{
		Use:   "inject-context",
		Short: "Print relevant learnings as a context block to stdout",
		Long:  "Queries the learning database and prints matching learnings as a markdown block for shell injection into a worker prompt. When --query is omitted, cold-start mode ranks by quality × recency. Set NANIKA_NO_INJECT=1 to suppress output entirely.",
		RunE:  runInjectContext,
	}
	injectCtxCmd.Flags().String("query", "", "query describing the current task context (omit for cold-start ranking by quality × recency)")
	injectCtxCmd.Flags().Int("limit", 10, "max learnings to include")
	injectCtxCmd.Flags().Int("max-bytes", 0, "truncate output to this many bytes (0 = unlimited)")

	snapshotCmd := &cobra.Command{
		Use:   "snapshot-session",
		Short: "Capture learnings from a workspace session into the database",
		Long:  "Scans worker output.md files in the given workspace (default: most recent) and stores extracted learnings in the learning database.",
		RunE:  runSnapshotSession,
	}
	snapshotCmd.Flags().String("workspace", "", "workspace ID or path (default: most recent)")

	preflightCmd := &cobra.Command{
		Use:   "preflight",
		Short: "Print a preflight brief (scheduler, tracker, learnings, …) for worker sessions",
		Long:  "Assembles a system-state brief from registered preflight sections and prints it to stdout. When no sections are registered the output is empty. Set NANIKA_NO_INJECT=1 to suppress output entirely.",
		RunE:  runPreflight,
	}
	preflightCmd.Flags().Int("max-bytes", 6144, "truncate output to this many bytes (0 = unlimited)")
	preflightCmd.Flags().StringSlice("sections", nil, "only include these sections (comma-separated; empty = all)")
	preflightCmd.Flags().String("format", "text", "output format: text or json")

	hooksCmd.AddCommand(flushCtxCmd, injectCtxCmd, snapshotCmd, preflightCmd)
	rootCmd.AddCommand(hooksCmd)
}

func runFlushContext(cmd *cobra.Command, args []string) error {
	query, _ := cmd.Flags().GetString("query")
	output, _ := cmd.Flags().GetString("output")
	limit, _ := cmd.Flags().GetInt("limit")

	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	embedder := learning.NewEmbedder(learning.LoadAPIKey())
	ctx, cancel := context.WithTimeout(cmd.Context(), 10*time.Second)
	defer cancel()

	if err := learning.FlushContext(ctx, db, embedder, query, domain, limit, output); err != nil {
		return err
	}

	fmt.Printf("context bundle written to %s\n", output)
	return nil
}

func runInjectContext(cmd *cobra.Command, args []string) error {
	if os.Getenv("NANIKA_NO_INJECT") == "1" {
		return nil
	}

	query, _ := cmd.Flags().GetString("query")
	limit, _ := cmd.Flags().GetInt("limit")
	maxBytes, _ := cmd.Flags().GetInt("max-bytes")

	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	embedder := learning.NewEmbedder(learning.LoadAPIKey())
	ctx, cancel := context.WithTimeout(cmd.Context(), 5*time.Second)
	defer cancel()

	content, err := learning.InjectContext(ctx, db, embedder, query, domain, limit)
	if err != nil {
		return err
	}

	if maxBytes > 0 && len(content) > maxBytes {
		content = content[:maxBytes]
		// Trim to the last complete line to avoid a mid-line cutoff.
		if idx := strings.LastIndex(content, "\n"); idx > 0 {
			content = content[:idx+1]
		}
	}

	if content != "" {
		fmt.Print(content)
	}
	return nil
}

func runSnapshotSession(cmd *cobra.Command, args []string) error {
	workspaceArg, _ := cmd.Flags().GetString("workspace")

	var wsPath string
	if workspaceArg != "" {
		if resolved, err := core.ResolveWorkspacePath(workspaceArg); err == nil {
			wsPath = resolved
		} else if _, err := os.Stat(workspaceArg); err == nil {
			wsPath = workspaceArg
		} else {
			return fmt.Errorf("workspace %q not found", workspaceArg)
		}
	} else {
		workspaces, err := core.ListWorkspaces()
		if err != nil {
			return err
		}
		if len(workspaces) == 0 {
			fmt.Println("no workspaces found")
			return nil
		}
		wsPath = workspaces[0]
	}

	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	embedder := learning.NewEmbedder(learning.LoadAPIKey())
	ctx, cancel := context.WithTimeout(cmd.Context(), 15*time.Second)
	defer cancel()

	dom := domain
	if cp, _ := core.LoadCheckpoint(wsPath); cp != nil && cp.Domain != "" {
		dom = cp.Domain
	}

	n, err := learning.SnapshotSession(ctx, db, embedder, wsPath, dom)
	if err != nil {
		return err
	}

	fmt.Printf("snapshot: captured %d learnings from %s\n", n, filepath.Base(wsPath))
	return nil
}

func runPreflight(cmd *cobra.Command, args []string) error {
	if os.Getenv("NANIKA_NO_INJECT") == "1" {
		return nil
	}

	maxBytes, _ := cmd.Flags().GetInt("max-bytes")
	sections, _ := cmd.Flags().GetStringSlice("sections")
	format, _ := cmd.Flags().GetString("format")

	// maxBytes == 0 means "unlimited" per the flag's help text. The cobra default
	// is 6144 when the flag is not provided on the command line, so reaching this
	// branch with 0 means the user explicitly opted into an unbounded brief.

	switch format {
	case "", "text", "json":
	default:
		return fmt.Errorf("unknown --format %q (expected text or json)", format)
	}

	ctx, cancel := context.WithTimeout(cmd.Context(), 5*time.Second)
	defer cancel()

	brief := preflight.BuildBrief(ctx, sections)

	// For text format, apply capacity constraints and drop lowest-priority
	// sections as needed. JSON format is not truncated by design (full state
	// is useful for automation).
	var out string
	if format == "json" {
		// JSON mode always emits a valid document so downstream
		// parsers never see an empty stdin. Blocks is initialized to
		// a non-nil empty slice by BuildBrief.
		data, err := json.Marshal(brief)
		if err != nil {
			return fmt.Errorf("marshal brief: %w", err)
		}
		out = string(data)
	} else {
		// Text mode: compose with capacity constraints and use the
		// pre-rendered markdown to avoid redundant rendering.
		_, dropped, rendered := brief.ComposeWithCapacity(maxBytes)
		if len(dropped) > 0 {
			fmt.Fprintf(os.Stderr, "preflight: dropped sections to fit capacity: %s\n", strings.Join(dropped, ", "))
		}
		out = rendered
	}

	if out != "" {
		fmt.Print(out)
	}
	return nil
}
