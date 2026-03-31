package scan

import (
	"testing"
)

func TestParseReviewBlockers_StandardBullets(t *testing.T) {
	markdown := `# Review

## Blockers

- First blocker item
- Second blocker item with **[main.go:42]**
- Third item in backticks with ` + "`" + `utils.go:99` + "`" + `
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 3 {
		t.Fatalf("Expected 3 blockers, got %d", len(blockers))
	}

	if blockers[0].Title != "First blocker item" {
		t.Errorf("blockers[0].Title = %q, want %q", blockers[0].Title, "First blocker item")
	}
	if blockers[0].File != "" {
		t.Errorf("blockers[0].File = %q, want empty", blockers[0].File)
	}

	if blockers[1].Title != "Second blocker item with **[main.go:42]**" {
		t.Errorf("blockers[1].Title = %q", blockers[1].Title)
	}
	if blockers[1].File != "main.go" {
		t.Errorf("blockers[1].File = %q, want %q", blockers[1].File, "main.go")
	}
	if blockers[1].LineStart != 42 || blockers[1].LineEnd != 42 {
		t.Errorf("blockers[1] line range = %d-%d, want 42-42", blockers[1].LineStart, blockers[1].LineEnd)
	}

	if blockers[2].File != "utils.go" {
		t.Errorf("blockers[2].File = %q, want %q", blockers[2].File, "utils.go")
	}
	if blockers[2].LineStart != 99 {
		t.Errorf("blockers[2].LineStart = %d, want 99", blockers[2].LineStart)
	}
}

func TestParseReviewBlockers_NumberedList(t *testing.T) {
	markdown := `## Blockers

1. First numbered blocker
2. Second numbered blocker with **[handler.go:10-20]**
3. Third item with ` + "`" + `api/routes.go:5` + "`" + `
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 3 {
		t.Fatalf("Expected 3 blockers, got %d", len(blockers))
	}

	if blockers[0].Title != "First numbered blocker" {
		t.Errorf("blockers[0].Title = %q, want %q", blockers[0].Title, "First numbered blocker")
	}

	if blockers[1].File != "handler.go" {
		t.Errorf("blockers[1].File = %q, want %q", blockers[1].File, "handler.go")
	}
	if blockers[1].LineStart != 10 || blockers[1].LineEnd != 20 {
		t.Errorf("blockers[1] line range = %d-%d, want 10-20", blockers[1].LineStart, blockers[1].LineEnd)
	}

	if blockers[2].File != "api/routes.go" {
		t.Errorf("blockers[2].File = %q, want %q", blockers[2].File, "api/routes.go")
	}
}

func TestParseReviewBlockers_NoBlockersSection(t *testing.T) {
	markdown := `# Review

## Summary

This is just a summary with no blockers.

## Notes

- Some notes here
`

	blockers := ParseReviewBlockers(markdown)

	if blockers != nil && len(blockers) != 0 {
		t.Fatalf("Expected empty blockers, got %d", len(blockers))
	}
}

func TestParseReviewBlockers_CodeBlockWithHeadingLike(t *testing.T) {
	markdown := `## Blockers

- First real blocker

` + "```" + `go
## This looks like a heading
but it's inside a code block
- This should not be parsed as a blocker
` + "```" + `

- Second real blocker after code block
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 2 {
		t.Fatalf("Expected 2 blockers, got %d", len(blockers))
	}

	if blockers[0].Title != "First real blocker" {
		t.Errorf("blockers[0].Title = %q", blockers[0].Title)
	}

	if blockers[1].Title != "Second real blocker after code block" {
		t.Errorf("blockers[1].Title = %q", blockers[1].Title)
	}
}

func TestParseReviewBlockers_MultipleFileReferences(t *testing.T) {
	markdown := `## Blockers

- Item with multiple formats: **[server.go:15]** and also ` + "`" + `client.go:25` + "`" + `
- Another with range **[config.go:100-150]**
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 2 {
		t.Fatalf("Expected 2 blockers, got %d", len(blockers))
	}

	// First item should extract from the first file reference found
	if blockers[0].File != "server.go" {
		t.Errorf("blockers[0].File = %q, want %q", blockers[0].File, "server.go")
	}
	if blockers[0].LineStart != 15 {
		t.Errorf("blockers[0].LineStart = %d, want 15", blockers[0].LineStart)
	}

	// Second item should extract range
	if blockers[1].File != "config.go" {
		t.Errorf("blockers[1].File = %q, want %q", blockers[1].File, "config.go")
	}
	if blockers[1].LineStart != 100 || blockers[1].LineEnd != 150 {
		t.Errorf("blockers[1] range = %d-%d, want 100-150", blockers[1].LineStart, blockers[1].LineEnd)
	}
}

func TestParseReviewBlockers_MalformedIncompleteSection(t *testing.T) {
	markdown := `## Blockers

- Valid blocker

Incomplete list without proper formatting
This is just text, not a list item

- Another valid blocker

## Next Section
`

	blockers := ParseReviewBlockers(markdown)

	// Should only get valid list items
	if len(blockers) != 2 {
		t.Fatalf("Expected 2 blockers, got %d", len(blockers))
	}

	if blockers[0].Title != "Valid blocker" {
		t.Errorf("blockers[0].Title = %q", blockers[0].Title)
	}

	if blockers[1].Title != "Another valid blocker" {
		t.Errorf("blockers[1].Title = %q", blockers[1].Title)
	}
}

func TestParseReviewBlockers_CaseInsensitiveHeader(t *testing.T) {
	tests := []struct {
		header string
		name   string
	}{
		{"## Blockers", "lowercase"},
		{"## BLOCKERS", "uppercase"},
		{"## Blockers", "titlecase"},
		{"# Blockers", "h1 header"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			markdown := tt.header + "\n\n- Test blocker\n"
			blockers := ParseReviewBlockers(markdown)

			if len(blockers) != 1 {
				t.Fatalf("Expected 1 blocker, got %d", len(blockers))
			}

			if blockers[0].Title != "Test blocker" {
				t.Errorf("Title = %q, want %q", blockers[0].Title, "Test blocker")
			}
		})
	}
}

func TestParseReviewBlockers_EmptyBlockersSection(t *testing.T) {
	markdown := `## Blockers

## Next Section

Content here
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 0 {
		t.Fatalf("Expected 0 blockers, got %d", len(blockers))
	}
}

func TestParseReviewBlockers_LargeLineRange(t *testing.T) {
	markdown := `## Blockers

- Fix issue in **[migration.sql:1-5000]**
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 1 {
		t.Fatalf("Expected 1 blocker, got %d", len(blockers))
	}

	if blockers[0].File != "migration.sql" {
		t.Errorf("File = %q, want %q", blockers[0].File, "migration.sql")
	}

	if blockers[0].LineStart != 1 || blockers[0].LineEnd != 5000 {
		t.Errorf("Range = %d-%d, want 1-5000", blockers[0].LineStart, blockers[0].LineEnd)
	}
}

func TestParseReviewBlockers_IndentedListItems(t *testing.T) {
	markdown := `## Blockers

  - Indented bullet item with **[app.go:30]**
    - Nested item (should not be parsed as top-level)

  2. Indented numbered item with ` + "`" + `service.go:40` + "`" + `
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) < 1 {
		t.Fatalf("Expected at least 1 blocker, got %d", len(blockers))
	}

	if blockers[0].File != "app.go" {
		t.Errorf("blockers[0].File = %q, want app.go", blockers[0].File)
	}
}

func TestParseReviewBlockers_FilePathsWithDirs(t *testing.T) {
	markdown := `## Blockers

- Update **[internal/handlers/user.go:100]**
- Fix **[pkg/database/migrations/001_init.sql:1-50]**
- Check ` + "`" + `./utils/helpers.go:25` + "`" + `
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 3 {
		t.Fatalf("Expected 3 blockers, got %d", len(blockers))
	}

	if blockers[0].File != "internal/handlers/user.go" {
		t.Errorf("blockers[0].File = %q", blockers[0].File)
	}

	if blockers[1].File != "pkg/database/migrations/001_init.sql" {
		t.Errorf("blockers[1].File = %q", blockers[1].File)
	}

	if blockers[2].File != "./utils/helpers.go" {
		t.Errorf("blockers[2].File = %q", blockers[2].File)
	}
}

func TestParseReviewBlockers_StopsAtNextHeading(t *testing.T) {
	markdown := `## Blockers

- Blocker one
- Blocker two

## Another Section

- This should not be a blocker
- Neither should this

## Yet Another Section

- Also not a blocker
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 2 {
		t.Fatalf("Expected 2 blockers, got %d", len(blockers))
	}

	if blockers[0].Title != "Blocker one" {
		t.Errorf("blockers[0].Title = %q", blockers[0].Title)
	}

	if blockers[1].Title != "Blocker two" {
		t.Errorf("blockers[1].Title = %q", blockers[1].Title)
	}
}

func TestParseReviewBlockers_MixedBulletAndNumbered(t *testing.T) {
	markdown := `## Blockers

- Bullet item one
1. Numbered item one
- Bullet item two
2. Numbered item two with **[file.go:10]**
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 4 {
		t.Fatalf("Expected 4 blockers, got %d", len(blockers))
	}

	if blockers[0].Title != "Bullet item one" {
		t.Errorf("blockers[0] = %q", blockers[0].Title)
	}

	if blockers[1].Title != "Numbered item one" {
		t.Errorf("blockers[1] = %q", blockers[1].Title)
	}

	if blockers[3].File != "file.go" {
		t.Errorf("blockers[3].File = %q", blockers[3].File)
	}
}

func TestParseReviewBlockers_MultilineCodeBlock(t *testing.T) {
	markdown := `## Blockers

- Item before code block

` + "```" + `markdown
## This is a fake header
- This is a fake blocker item
- Another fake one with **[fake.go:99]**
` + "```" + `

- Item after code block
`

	blockers := ParseReviewBlockers(markdown)

	if len(blockers) != 2 {
		t.Fatalf("Expected 2 blockers, got %d", len(blockers))
	}

	if blockers[0].Title != "Item before code block" {
		t.Errorf("blockers[0] = %q", blockers[0].Title)
	}

	if blockers[1].Title != "Item after code block" {
		t.Errorf("blockers[1] = %q", blockers[1].Title)
	}
}
