package cmd

import (
	"fmt"
	"strings"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

func init() {
	memoryCmd := &cobra.Command{
		Use:   "memory",
		Short: "Manage persona memory entries",
		Long: `Commands for managing persona memory entries.

Examples:
  orchestrator memory promote senior-backend-engineer
  orchestrator memory promote senior-backend-engineer --match "golang"
  orchestrator memory promote senior-backend-engineer --used 3
  orchestrator memory sync`,
	}

	promoteCmd := &cobra.Command{
		Use:   "promote <persona>",
		Short: "Promote persona memory entries to global MEMORY.md",
		Long: `Reads the persona's MEMORY.md, selects matching non-superseded entries,
appends them to ~/.alluka/memory/global.md, and removes them from the persona file.

By default (no flags) all non-superseded entries are promoted.
Use --match to filter by content substring, or --used to filter by minimum used count.

Examples:
  # Promote all entries for a persona
  orchestrator memory promote senior-backend-engineer

  # Promote only entries whose content contains "golang"
  orchestrator memory promote senior-backend-engineer --match "golang"

  # Promote entries that have been used at least 3 times
  orchestrator memory promote senior-backend-engineer --used 3`,
		Args: cobra.ExactArgs(1),
		RunE: runMemoryPromote,
	}
	promoteCmd.Flags().String("match", "", "only promote entries whose content contains this substring (case-insensitive)")
	promoteCmd.Flags().Int("used", 0, "only promote entries with used count >= this value (0 = no filter)")

	syncCmd := &cobra.Command{
		Use:   "sync",
		Short: "Sync high-quality learnings from learning DB to persona MEMORY.md files",
		Long: `Queries the learning database for entries with quality_score > 0.7 that have
not yet been promoted, converts them to MemoryEntry format, and merges them into
the appropriate persona MEMORY.md files based on worker_name. Duplicate entries
(same normalized content) are skipped. Once synced, learnings are marked as
promoted to prevent duplicate promotion.

Examples:
  orchestrator memory sync`,
		RunE: runMemorySync,
	}

	searchCmd := &cobra.Command{
		Use:   "search <query>",
		Short: "Search learning database with FTS5",
		Long: `Searches the learning database using hybrid FTS5 + semantic search.
Results are ranked by relevance and quality, formatted as:

  [quality] date persona: content

Examples:
  orchestrator memory search "golang error handling"
  orchestrator memory search "testing patterns" --limit 5`,
		Args: cobra.ExactArgs(1),
		RunE: runMemorySearch,
	}
	searchCmd.Flags().Int("limit", 10, "maximum number of results to return")

	memoryCmd.AddCommand(promoteCmd)
	memoryCmd.AddCommand(syncCmd)
	memoryCmd.AddCommand(searchCmd)
	rootCmd.AddCommand(memoryCmd)
}

func runMemoryPromote(cmd *cobra.Command, args []string) error {
	personaName := args[0]
	match, _ := cmd.Flags().GetString("match")
	usedMin, _ := cmd.Flags().GetInt("used")

	matcher := buildMatcher(match, usedMin)
	n, err := worker.PromotePersonaEntries(personaName, matcher)
	if err != nil {
		return fmt.Errorf("promoting entries for %q: %w", personaName, err)
	}

	if n == 0 {
		fmt.Printf("no entries matched for persona %q\n", personaName)
	} else {
		fmt.Printf("promoted %d %s from %q to global MEMORY.md\n", n, pluralEntry(n), personaName)
	}
	return nil
}

// buildMatcher returns a matcher function for PromotePersonaEntries.
// Returns nil when no filters are set (all entries pass).
func buildMatcher(match string, usedMin int) func(*worker.MemoryEntry) bool {
	if match == "" && usedMin == 0 {
		return nil
	}
	matchLower := strings.ToLower(match)
	return func(e *worker.MemoryEntry) bool {
		if usedMin > 0 && e.Used < usedMin {
			return false
		}
		if matchLower != "" && !strings.Contains(strings.ToLower(e.Content), matchLower) {
			return false
		}
		return true
	}
}

func pluralEntry(n int) string {
	if n == 1 {
		return "entry"
	}
	return "entries"
}

func runMemorySync(cmd *cobra.Command, args []string) error {
	n, err := worker.SyncLearningsToPersonas(cmd.Context())
	if err != nil {
		return fmt.Errorf("syncing learnings to personas: %w", err)
	}

	if n == 0 {
		fmt.Println("no promotable learnings found")
	} else {
		fmt.Printf("synced %d %s from learning DB to persona MEMORY.md files\n", n, pluralEntry(n))
	}
	return nil
}

func runMemorySearch(cmd *cobra.Command, args []string) error {
	query := args[0]
	limit, _ := cmd.Flags().GetInt("limit")

	results, err := worker.SearchLearnings(cmd.Context(), query, limit)
	if err != nil {
		return fmt.Errorf("searching learnings: %w", err)
	}

	if len(results) == 0 {
		fmt.Println("no learnings found")
		return nil
	}

	for _, result := range results {
		fmt.Println(result)
	}
	return nil
}
