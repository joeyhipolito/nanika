package cmd

import (
	"fmt"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// CreateOptions holds all options for creating a note.
type CreateOptions struct {
	Title      string
	Type       string
	ContextSet string
	Status     string
	Summary    string
	Tags       []string
	Template   string // vault-relative path to a note whose body is used as the note body
}

// CreateOutput represents the JSON output format for the create command.
type CreateOutput struct {
	Path  string `json:"path"`
	Title string `json:"title,omitempty"`
}

// CreateCmd creates a new note in the vault with optional frontmatter.
// Frontmatter fields are written in a deterministic order.
func CreateCmd(vaultPath, notePath string, opts CreateOptions, jsonOutput bool) error {
	content, err := buildCreateContent(vaultPath, opts)
	if err != nil {
		return err
	}

	if err := vault.WriteNote(vaultPath, notePath, content); err != nil {
		return err
	}

	if jsonOutput {
		return output.JSON(CreateOutput{
			Path:  notePath,
			Title: opts.Title,
		})
	}

	fmt.Printf("Created %s\n", notePath)
	return nil
}

// buildCreateContent assembles the full note content from options.
func buildCreateContent(vaultPath string, opts CreateOptions) (string, error) {
	hasFrontmatter := opts.Title != "" || opts.Type != "" || opts.ContextSet != "" ||
		opts.Status != "" || opts.Summary != "" || len(opts.Tags) > 0

	var b strings.Builder

	if hasFrontmatter {
		b.WriteString("---\n")
		if opts.Title != "" {
			fmt.Fprintf(&b, "title: %s\n", opts.Title)
		}
		fmt.Fprintf(&b, "created: %s\n", time.Now().Format("2006-01-02"))
		if opts.Type != "" {
			fmt.Fprintf(&b, "type: %s\n", opts.Type)
		}
		if opts.Status != "" {
			fmt.Fprintf(&b, "status: %s\n", opts.Status)
		}
		if opts.ContextSet != "" {
			fmt.Fprintf(&b, "context-set: %s\n", opts.ContextSet)
		}
		if opts.Summary != "" {
			// Quote the summary if it contains YAML-special characters.
			if strings.ContainsAny(opts.Summary, ":{}[]#&*!|>'\"%@`") {
				fmt.Fprintf(&b, "summary: \"%s\"\n", strings.ReplaceAll(opts.Summary, "\"", "\\\""))
			} else {
				fmt.Fprintf(&b, "summary: %s\n", opts.Summary)
			}
		}
		if len(opts.Tags) > 0 {
			b.WriteString("tags:\n")
			for _, tag := range opts.Tags {
				fmt.Fprintf(&b, "  - %s\n", tag)
			}
		}
		b.WriteString("---\n")
	}

	if opts.Template != "" {
		body, err := loadTemplateBody(vaultPath, opts.Template)
		if err != nil {
			return "", err
		}
		if hasFrontmatter {
			b.WriteByte('\n')
		}
		b.WriteString(body)
		if !strings.HasSuffix(body, "\n") {
			b.WriteByte('\n')
		}
	} else if opts.Title != "" {
		b.WriteString("\n# ")
		b.WriteString(opts.Title)
		b.WriteByte('\n')
	}

	return b.String(), nil
}

// loadTemplateBody reads a vault note at templatePath and returns its body
// (frontmatter stripped). Returns an error if the file cannot be read.
func loadTemplateBody(vaultPath, templatePath string) (string, error) {
	note, err := vault.ReadNote(vaultPath, templatePath)
	if err != nil {
		return "", fmt.Errorf("loading template %q: %w", templatePath, err)
	}
	return note.Body, nil
}
