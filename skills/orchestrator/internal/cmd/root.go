package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

var (
	verbose    bool
	dryRun     bool
	domain     string
	model      string
	sequential bool
	personaDir string
	nanikaDir  string
	maxTurns   int
)

var rootCmd = &cobra.Command{
	Use:   "orchestrator",
	Short: "Multi-agent mission orchestrator for Nanika",
	Long: `Orchestrator decomposes tasks into phases, spawns specialized workers,
and coordinates their execution. Each worker gets a persona, skills,
and context — then runs as a Claude session in its own directory.

Folders are workers. CLAUDE.md is personality. Execution is just
running Claude CLI in a directory.`,
}

func init() {
	rootCmd.PersistentFlags().BoolVarP(&verbose, "verbose", "v", false, "verbose output")
	rootCmd.PersistentFlags().BoolVar(&dryRun, "dry-run", false, "show plan without executing")
	rootCmd.PersistentFlags().StringVar(&domain, "domain", "dev", "task domain (dev/personal/work/creative/academic)")
	rootCmd.PersistentFlags().StringVar(&model, "model", "", "force model for all workers")
	rootCmd.PersistentFlags().BoolVar(&sequential, "sequential", false, "force sequential execution")
	rootCmd.PersistentFlags().StringVar(&personaDir, "personas-dir", "", "path to personas directory (default: ~/nanika/personas/)")
	rootCmd.PersistentFlags().StringVar(&nanikaDir, "nanika-dir", "", "path to nanika directory (default: ~/nanika)")
	rootCmd.PersistentFlags().IntVar(&maxTurns, "max-turns", 50, "max agentic turns per worker")

	// Apply config overrides before any command runs
	rootCmd.PersistentPreRun = func(cmd *cobra.Command, args []string) {
		if personaDir != "" {
			persona.SetDir(personaDir)
		}
		if nanikaDir != "" {
			worker.SetNanikaDir(nanikaDir)
		}
	}
}

// Execute runs the root command.
func Execute() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
