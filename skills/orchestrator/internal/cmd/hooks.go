package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
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

	hooksCmd.AddCommand(flushCtxCmd, injectCtxCmd, snapshotCmd)
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
