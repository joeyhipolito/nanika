package cmd

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

func init() {
	ingestCmd := &cobra.Command{
		Use:   "ingest",
		Short: "Ingest external content into the learnings database",
		Long: `Ingest walks external content sources (docs trees, reference material) and
stores them as TypeSource learnings. Dedup is handled by the existing DB.Insert
cosine-similarity layer; re-ingesting the same content is idempotent.`,
	}

	docsCmd := &cobra.Command{
		Use:   "docs",
		Short: "Walk a docs directory and ingest .md files as source learnings",
		Long: `Walks the given directory for *.md files, splits each file by level-2
headings into chunks, and stores one learning per chunk. Deterministic chunk
IDs (sha256 of rel-path#slug + domain) make re-ingestion idempotent.

Default root is ~/nanika/docs.

Exit codes:
  0 — success
  1 — per-file errors were encountered, or an operational failure occurred`,
		RunE: runIngestDocs,
	}
	docsCmd.Flags().String("root", "", "directory to walk for .md files (default: ~/nanika/docs)")
	docsCmd.Flags().Bool("json", false, "emit stats as JSON instead of a human-readable summary")

	ingestCmd.AddCommand(docsCmd)
	rootCmd.AddCommand(ingestCmd)
}

func runIngestDocs(cmd *cobra.Command, args []string) error {
	cmd.SilenceUsage = true

	root, _ := cmd.Flags().GetString("root")
	asJSON, _ := cmd.Flags().GetBool("json")

	if root == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return fmt.Errorf("resolving home dir: %w", err)
		}
		root = filepath.Join(home, "nanika", "docs")
	}

	if _, err := os.Stat(root); err != nil {
		return fmt.Errorf("ingest root %q: %w", root, err)
	}

	if dryRun {
		return runIngestDocsDryRun(cmd, root, asJSON)
	}

	dbPath, err := learningsDBPath()
	if err != nil {
		return err
	}

	ldb, err := learning.OpenDB(dbPath)
	if err != nil {
		return fmt.Errorf("opening learnings DB: %w", err)
	}
	defer ldb.Close()

	embedder := learning.NewEmbedder(learning.LoadAPIKey())

	stats, err := learning.IngestDocs(root, ldb, embedder)
	if err != nil {
		return fmt.Errorf("ingest: %w", err)
	}

	if asJSON {
		enc := json.NewEncoder(cmd.OutOrStdout())
		enc.SetIndent("", "  ")
		if err := enc.Encode(stats); err != nil {
			return fmt.Errorf("encoding stats: %w", err)
		}
	} else {
		fmt.Fprintf(cmd.OutOrStdout(),
			"ingest docs: root=%s files=%d chunks=%d deduped=%d errors=%d\n",
			root, stats.FilesScanned, stats.ChunksCreated, stats.ChunksSkippedDedup, len(stats.Errors))
		for _, e := range stats.Errors {
			fmt.Fprintf(cmd.ErrOrStderr(), "  error: %s\n", e)
		}
	}

	if len(stats.Errors) > 0 {
		return fmt.Errorf("ingest completed with %d error(s)", len(stats.Errors))
	}
	return nil
}

func runIngestDocsDryRun(cmd *cobra.Command, root string, asJSON bool) error {
	var mdFiles []string
	walkErr := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}
		if strings.HasSuffix(strings.ToLower(d.Name()), ".md") {
			rel, relErr := filepath.Rel(root, path)
			if relErr != nil {
				rel = path
			}
			mdFiles = append(mdFiles, rel)
		}
		return nil
	})
	if walkErr != nil {
		return fmt.Errorf("scanning %q: %w", root, walkErr)
	}

	if asJSON {
		plan := struct {
			Root   string   `json:"root"`
			Count  int      `json:"count"`
			Files  []string `json:"files"`
			DryRun bool     `json:"dry_run"`
		}{Root: root, Count: len(mdFiles), Files: mdFiles, DryRun: true}
		enc := json.NewEncoder(cmd.OutOrStdout())
		enc.SetIndent("", "  ")
		if err := enc.Encode(plan); err != nil {
			return fmt.Errorf("encoding plan: %w", err)
		}
		return nil
	}

	fmt.Fprintf(cmd.OutOrStdout(), "ingest docs (dry-run)\n")
	fmt.Fprintf(cmd.OutOrStdout(), "  root:  %s\n", root)
	fmt.Fprintf(cmd.OutOrStdout(), "  files: %d markdown file(s) would be walked\n", len(mdFiles))
	for _, f := range mdFiles {
		fmt.Fprintf(cmd.OutOrStdout(), "    %s\n", f)
	}
	return nil
}
