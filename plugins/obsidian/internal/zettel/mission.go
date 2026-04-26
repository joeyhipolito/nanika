package zettel

import (
	"fmt"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// Mission holds the data needed to render a mission Zettel.
type Mission struct {
	ID        string
	Slug      string
	Completed time.Time
	Personas  []string
	Trackers  []string
	Artifacts []string
}

// RenderMission converts a Mission into an Obsidian-flavored markdown note.
// The rendered note always contains exactly 8 frontmatter fields:
// type, id, slug, status, completed, personas, trackers, artifacts.
func RenderMission(m Mission) string {
	var b strings.Builder

	date := ""
	if !m.Completed.IsZero() {
		date = m.Completed.UTC().Format("2006-01-02")
	}

	// Frontmatter — 8 fields in deterministic order.
	b.WriteString("---\n")
	fmt.Fprintf(&b, "type: %s\n", vault.TypeMission)
	fmt.Fprintf(&b, "id: %s\n", yamlString(m.ID))
	fmt.Fprintf(&b, "slug: %s\n", yamlString(m.Slug))
	b.WriteString("status: completed\n")
	if date != "" {
		fmt.Fprintf(&b, "completed: %s\n", date)
	} else {
		b.WriteString("completed: \"\"\n")
	}
	fmStringList(&b, "personas", m.Personas)
	fmStringList(&b, "trackers", m.Trackers)
	fmStringList(&b, "artifacts", m.Artifacts)
	b.WriteString("---\n")

	// Title uses slug when present, falls back to ID.
	title := m.Slug
	if title == "" {
		title = m.ID
	}
	fmt.Fprintf(&b, "\n# %s\n", title)

	if date != "" {
		fmt.Fprintf(&b, "\nCompleted on %s.\n", date)
	} else {
		b.WriteString("\nCompleted.\n")
	}

	if len(m.Personas) > 0 {
		b.WriteString("\n## Personas\n\n")
		for _, p := range m.Personas {
			fmt.Fprintf(&b, "- [[%s]]\n", p)
		}
	}

	if len(m.Trackers) > 0 {
		b.WriteString("\n## Trackers\n\n")
		for _, tk := range m.Trackers {
			fmt.Fprintf(&b, "- [[%s]]\n", tk)
		}
	}

	if len(m.Artifacts) > 0 {
		b.WriteString("\n## Artifacts\n\n")
		for _, a := range m.Artifacts {
			// Use filename stem as display alias for wikilinks.
			stem := strings.TrimSuffix(a, filepath.Ext(a))
			if stem != a {
				fmt.Fprintf(&b, "- [[%s|%s]]\n", a, stem)
			} else {
				fmt.Fprintf(&b, "- [[%s]]\n", a)
			}
		}
	}

	return b.String()
}

// fmStringList writes a YAML list field. Empty or nil slices emit "key: []".
// Values are quoted when they contain YAML special characters.
func fmStringList(b *strings.Builder, key string, values []string) {
	if len(values) == 0 {
		fmt.Fprintf(b, "%s: []\n", key)
		return
	}
	fmt.Fprintf(b, "%s:\n", key)
	for _, v := range values {
		fmt.Fprintf(b, "  - %s\n", yamlString(v))
	}
}

// yamlString quotes s with double quotes when it contains YAML special
// characters that would produce invalid or ambiguous YAML if unquoted.
func yamlString(s string) string {
	if strings.ContainsAny(s, `:{}[]#&*!|>'"@`+"`"+`%`) {
		return `"` + strings.ReplaceAll(s, `"`, `\"`) + `"`
	}
	return s
}
