package cmd

import (
	"fmt"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

func init() {
	templatesCmd := &cobra.Command{
		Use:   "templates",
		Short: "Manage mission templates",
		Long: `Templates are frozen plans saved from previous missions.
Use --save-template on a run to create one, then --template to reuse it.`,
	}

	listCmd := &cobra.Command{
		Use:   "list",
		Short: "List available templates",
		RunE:  listTemplates,
	}

	templatesCmd.AddCommand(listCmd)
	rootCmd.AddCommand(templatesCmd)
}

func listTemplates(cmd *cobra.Command, args []string) error {
	templates, err := core.ListTemplates()
	if err != nil {
		return fmt.Errorf("list templates: %w", err)
	}

	if len(templates) == 0 {
		fmt.Println("no templates found")
		fmt.Println("save one with: orchestrator run <task> --save-template <name>")
		return nil
	}

	fmt.Printf("templates: %d\n\n", len(templates))
	for _, t := range templates {
		// Strip newlines first, then truncate for display.
		task := singleLine(t.Task)
		if len(task) > 60 {
			task = task[:60] + "..."
		}

		fmt.Printf("  %-20s  %d phases (%s)  %s\n",
			t.Name, len(t.Phases), t.ExecutionMode, task)
	}

	return nil
}

func singleLine(s string) string {
	for i := range s {
		if s[i] == '\n' || s[i] == '\r' {
			return s[:i] + "..."
		}
	}
	return s
}
