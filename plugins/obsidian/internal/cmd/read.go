package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// ReadOutput represents the JSON output format for the read command.
type ReadOutput struct {
	Path        string          `json:"path"`
	Frontmatter map[string]any  `json:"frontmatter,omitempty"`
	Body        string          `json:"body"`
	Headings    []vault.Heading `json:"headings,omitempty"`
	Wikilinks   []string        `json:"wikilinks,omitempty"`
}

// ReadCmd reads a note's content from the vault.
// In JSON mode, returns parsed frontmatter, body, headings, and wikilinks.
// In text mode, prints the body content.
func ReadCmd(vaultPath, notePath string, jsonOutput bool) error {
	note, err := vault.ReadNote(vaultPath, notePath)
	if err != nil {
		return err
	}

	if jsonOutput {
		return output.JSON(ReadOutput{
			Path:        notePath,
			Frontmatter: note.Frontmatter,
			Body:        note.Body,
			Headings:    note.Headings,
			Wikilinks:   note.Wikilinks,
		})
	}

	fmt.Print(note.Body)
	return nil
}
