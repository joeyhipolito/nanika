package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// NotesCmd lists user notes or shows replies on a specific note.
func NotesCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags
	limit := 10
	var repliesFor int
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`substack notes - List your Notes or view replies

Usage:
  substack notes                  List your recent notes
  substack notes --limit 20       List more notes
  substack notes --replies <id>   Show replies on a note

Flags:
  --limit <N>        Number of notes to show (default: 10)
  --replies <id>     Show replies on a specific note
  --json             Output in JSON format
`)
			return nil
		case "--limit":
			if i+1 < len(args) {
				i++
				n := parseIntArg(args[i])
				if n > 0 {
					limit = n
				}
			}
		case "--replies":
			if i+1 >= len(args) {
				return fmt.Errorf("--replies requires a note ID")
			}
			i++
			repliesFor = parseIntArg(args[i])
			if repliesFor <= 0 {
				return fmt.Errorf("--replies requires a valid note ID")
			}
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	// Show replies for a specific note
	if repliesFor > 0 {
		return showNoteReplies(client, cfg.Subdomain, repliesFor, jsonOutput)
	}

	// List notes
	notes, err := client.ListNotes(limit)
	if err != nil {
		return fmt.Errorf("fetching notes: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(notes)
	}

	if len(notes) == 0 {
		fmt.Println("No notes found.")
		return nil
	}

	fmt.Printf("Notes (%d):\n\n", len(notes))
	for _, n := range notes {
		date := n.Date
		if len(date) > 10 {
			date = date[:10]
		}
		body := n.Body
		if len(body) > 100 {
			body = body[:100] + "..."
		}
		fmt.Printf("  %s  #%d", date, n.ID)
		if n.ChildrenCount > 0 {
			fmt.Printf("  (%d replies)", n.ChildrenCount)
		}
		fmt.Println()
		fmt.Printf("    %s\n", body)
		fmt.Printf("    https://substack.com/@%s/note/c-%d\n\n", cfg.Subdomain, n.ID)
	}

	return nil
}

func showNoteReplies(client *api.Client, subdomain string, noteID int, jsonOutput bool) error {
	result, err := client.GetNoteReplies(noteID)
	if err != nil {
		return fmt.Errorf("fetching replies: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(result)
	}

	// Show root note
	root := result.RootComment
	rootBody := root.Body
	if len(rootBody) > 200 {
		rootBody = rootBody[:200] + "..."
	}
	fmt.Printf("Note #%d by %s (%s):\n", root.ID, root.Name, root.Date[:10])
	fmt.Printf("  %s\n", rootBody)
	fmt.Printf("  https://substack.com/@%s/note/c-%d\n\n", subdomain, root.ID)

	if len(result.CommentBranches) == 0 {
		fmt.Println("  No replies.")
		return nil
	}

	fmt.Printf("Replies (%d):\n\n", len(result.CommentBranches))
	for _, branch := range result.CommentBranches {
		c := branch.Comment
		date := c.Date
		if len(date) > 10 {
			date = date[:10]
		}
		body := c.Body
		if len(body) > 150 {
			body = body[:150] + "..."
		}
		fmt.Printf("  %s  %s  #%d\n", date, c.Name, c.ID)
		fmt.Printf("    %s\n\n", body)
	}

	if result.MoreBranches > 0 {
		fmt.Printf("  ... and %d more replies\n", result.MoreBranches)
	}

	return nil
}
