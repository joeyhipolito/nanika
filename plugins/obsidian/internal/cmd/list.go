package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// ListOutput represents the JSON output format for the list command.
type ListOutput struct {
	Directory string           `json:"directory"`
	Notes     []vault.NoteInfo `json:"notes"`
	Count     int              `json:"count"`
}

// ListCmd lists notes in the vault or a subdirectory.
func ListCmd(vaultPath, dir string, jsonOutput bool) error {
	notes, err := vault.ListNotes(vaultPath, dir)
	if err != nil {
		return err
	}

	if jsonOutput {
		return output.JSON(ListOutput{
			Directory: dir,
			Notes:     notes,
			Count:     len(notes),
		})
	}

	if len(notes) == 0 {
		fmt.Println("No notes found.")
		return nil
	}

	for _, n := range notes {
		fmt.Println(n.Path)
	}
	fmt.Printf("\n%d note(s)\n", len(notes))
	return nil
}
