// Package cmd — `orchestrator barok status` subcommand.
//
// Surfaces the barok output-compression state to operators: which personas
// are eligible, how many bytes each persona's rule card consumes, and
// whether NANIKA_NO_BAROK=1 is currently short-circuiting injection.
//
// Used as a smoke test after deploy and as a one-liner diagnostic during
// Trigger-H investigations.
package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

func init() {
	barokCmd := &cobra.Command{
		Use:   "barok",
		Short: "Barok output-compression debug and status",
		Long: `Barok is the nanika-native output-compression variant that injects
a rule card into the worker CLAUDE.md on terminal phases for a small allow-list
of prose-heavy personas. See ~/.alluka/missions/artifacts/barok-design-delta.md
for the experiment window details.`,
	}

	statusCmd := &cobra.Command{
		Use:   "status",
		Short: "Print current barok configuration and per-persona rule-card byte sizes",
		RunE:  runBarokStatus,
	}

	barokCmd.AddCommand(statusCmd)
	rootCmd.AddCommand(barokCmd)
}

func runBarokStatus(cmd *cobra.Command, args []string) error {
	out := cmd.OutOrStdout()

	disabled := worker.BarokDisabled()
	envState := "enabled"
	if disabled {
		envState = fmt.Sprintf("DISABLED via %s=%s", worker.BarokEnvDisable, os.Getenv(worker.BarokEnvDisable))
	}

	fmt.Fprintln(out, "barok output-compression status")
	fmt.Fprintln(out, "================================")
	fmt.Fprintf(out, "env:      %s\n", envState)
	fmt.Fprintf(out, "personas: %d eligible\n", len(worker.BarokPersonas))
	fmt.Fprintln(out, "")
	fmt.Fprintln(out, "per-persona rule card:")
	fmt.Fprintf(out, "  %-24s %s\n", "persona", "rule-card bytes (terminal phase)")
	fmt.Fprintf(out, "  %-24s %s\n", "-------", "--------------------------------")
	for _, p := range worker.BarokPersonas {
		bytes := worker.BarokRuleCardBytes(p)
		fmt.Fprintf(out, "  %-24s %d\n", p, bytes)
	}
	fmt.Fprintln(out, "")
	fmt.Fprintln(out, "notes:")
	fmt.Fprintln(out, "  - bytes are zero when NANIKA_NO_BAROK=1 is set at invocation time.")
	fmt.Fprintln(out, "  - injection only fires for terminal phases in the mission DAG.")
	fmt.Fprintln(out, "  - non-terminal phases intentionally skip injection to preserve")
	fmt.Fprintln(out, "    prompt-prefix cache in downstream dependent workers.")

	return nil
}
