package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

// FindCmd runs symbolic retrieval against the compiled index.
func FindCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
		fmt.Print(`Usage: memory find <query> [--top <n>] [--json]

Examples:
  memory find "deploy notes"
  memory find "entity=alice"
  memory find "role=engineer project=alpha" --top 10
`)
		return nil
	}

	top := 5
	var queryArgs []string
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--top":
			i++
			if i >= len(args) {
				return fmt.Errorf("--top requires a value")
			}
			n, err := parsePositiveInt(args[i], 5)
			if err != nil {
				return err
			}
			top = n
		default:
			queryArgs = append(queryArgs, args[i])
		}
	}

	query, err := readTextInput(queryArgs)
	if err != nil {
		return err
	}
	if query == "" {
		return fmt.Errorf("find requires a query")
	}

	engine, err := store.Open()
	if err != nil {
		return err
	}
	result := engine.Find(query, top)

	if jsonOutput {
		return output.JSON(result)
	}

	if result.Count == 0 {
		fmt.Println("No matches.")
		return nil
	}
	for _, hit := range result.Hits {
		fmt.Printf("[%d] score=%.3f entity=%s source=%s\n", hit.ID, hit.Score, hit.Entity, hit.Source)
		fmt.Printf("  %s\n", hit.Preview)
	}
	return nil
}
