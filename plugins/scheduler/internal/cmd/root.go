// Package cmd contains all cobra command definitions for scheduler.
package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/spf13/cobra"
)

const version = "1.0.0"

// rootCmd is the top-level cobra command.
var rootCmd = &cobra.Command{
	Use:   "scheduler",
	Short: "A local job scheduler",
	Long: `scheduler — run and track scheduled commands on your machine.

Jobs are defined with a cron expression, stored in SQLite, and executed
by the scheduler daemon. Use 'scheduler configure' to get started.`,
	SilenceUsage:  true,
	SilenceErrors: true,
	Version:       version,
}

// Execute is the entry point called by main.
func Execute() {
	if err := rootCmd.Execute(); err != nil {
		for _, arg := range os.Args[1:] {
			if arg == "--json" {
				enc := json.NewEncoder(os.Stdout)
				enc.SetIndent("", "  ")
				_ = enc.Encode(map[string]string{"status": "error", "error": err.Error()})
				os.Exit(1)
			}
		}
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func init() {
	rootCmd.AddCommand(newConfigureCmd())
	rootCmd.AddCommand(newDoctorCmd())
	rootCmd.AddCommand(newJobsCmd())
	rootCmd.AddCommand(newRunCmd())
	rootCmd.AddCommand(newLogsCmd())
	rootCmd.AddCommand(newStatusCmd())
	rootCmd.AddCommand(newDaemonCmd())
	rootCmd.AddCommand(newHistoryCmd())
	rootCmd.AddCommand(newInitCmd())
	rootCmd.AddCommand(newQueryCmd())
}
