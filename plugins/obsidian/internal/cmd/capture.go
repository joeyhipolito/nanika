package cmd

import (
	"fmt"
	"io"
	"os"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// CaptureOutput represents the JSON output format for the capture command.
type CaptureOutput struct {
	Path   string `json:"path"`
	Source string `json:"source,omitempty"`
}

// CaptureCmd creates a fleeting note in the vault inbox from a body string or stdin.
// The note gets frontmatter: type: fleeting, created: YYYY-MM-DD, and optional source.
// Pass a vault.VaultKind as the optional last argument; omit for KindNanika (backward compat).
func CaptureCmd(vaultPath, body, source string, jsonOutput bool, kinds ...vault.VaultKind) error {
	kind := vault.KindNanika
	if len(kinds) > 0 {
		kind = kinds[0]
	}
	schema := vault.SchemaFor(kind)

	if body == "" {
		data, err := io.ReadAll(os.Stdin)
		if err != nil {
			return fmt.Errorf("reading stdin: %w", err)
		}
		body = strings.TrimRight(string(data), "\n")
		if body == "" {
			return fmt.Errorf("no body provided\n\nUsage: obsidian capture <body> [--source <url>]\n       echo 'text' | obsidian capture")
		}
	}

	now := time.Now()
	filename := fmt.Sprintf("%s/%s.md", schema.Inbox, now.Format("20060102-150405"))

	var b strings.Builder
	b.WriteString("---\n")
	fmt.Fprintf(&b, "type: %s\n", vault.TypeFleeting)
	fmt.Fprintf(&b, "created: %s\n", now.Format("2006-01-02"))
	if source != "" {
		fmt.Fprintf(&b, "source: %s\n", source)
	}
	b.WriteString("---\n\n")
	b.WriteString(body)
	if !strings.HasSuffix(body, "\n") {
		b.WriteByte('\n')
	}

	if err := vault.WriteNote(vaultPath, filename, b.String()); err != nil {
		return fmt.Errorf("writing capture note: %w", err)
	}

	if jsonOutput {
		return output.JSON(CaptureOutput{
			Path:   filename,
			Source: source,
		})
	}

	fmt.Printf("Captured to %s\n", filename)
	return nil
}
