package ingest

import (
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

// maxSupportedLearningsVersion is the highest schema_version this build can read.
const maxSupportedLearningsVersion = 1

// Learning represents a row from the learnings table.
type Learning struct {
	ID        string
	Type      string
	Content   string
	Context   string
	Domain    string
	AgentType string
	Tags      string
	SeenCount int
	UsedCount int
	CreatedAt string
	LastUsed  string
}

// LearningsIngestOptions controls the learnings ingest operation.
type LearningsIngestOptions struct {
	Domain string
	Since  time.Duration // 0 means all
	DryRun bool
}

// LearningsIngestResult holds the results of a learnings ingest.
type LearningsIngestResult struct {
	Created []string `json:"created"`
	Skipped []string `json:"skipped"`
	Errors  []string `json:"errors"`
	Source  string   `json:"source"`
}

// allukaHome returns the Alluka home directory, resolving in priority order:
// ALLUKA_HOME → VIA_HOME → ~/.alluka/
func allukaHome() (string, error) {
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return v, nil
	}
	if v := os.Getenv("VIA_HOME"); v != "" {
		return v, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("cannot determine home directory: %w", err)
	}
	return filepath.Join(home, ".alluka"), nil
}

// learningsDBPath returns the path to the learnings database.
func learningsDBPath() (string, error) {
	base, err := allukaHome()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "learnings.db"), nil
}

// IngestLearnings reads from the learnings SQLite DB and creates vault notes.
func IngestLearnings(vaultPath string, opts LearningsIngestOptions, state *State) (*LearningsIngestResult, error) {
	dbPath, err := learningsDBPath()
	if err != nil {
		return nil, err
	}

	if _, err := os.Stat(dbPath); err != nil {
		return nil, fmt.Errorf("learnings database not found at %s\n\nRun 'orchestrator learn' first", dbPath)
	}

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		return nil, fmt.Errorf("cannot open learnings database: %w", err)
	}
	defer db.Close()

	if err := checkLearningsSchemaVersion(db); err != nil {
		return nil, err
	}

	learnings, err := queryLearnings(db, opts)
	if err != nil {
		return nil, err
	}

	result := &LearningsIngestResult{Source: "learnings"}

	for _, l := range learnings {
		if state.HasLearning(l.ID) {
			result.Skipped = append(result.Skipped, l.ID)
			continue
		}

		notePath := learningNotePath(l)
		content := buildLearningNote(l)

		if opts.DryRun {
			result.Created = append(result.Created, notePath)
			continue
		}

		if err := writeNote(vaultPath, notePath, content); err != nil {
			if strings.Contains(err.Error(), "already exists") {
				result.Skipped = append(result.Skipped, notePath)
				state.MarkLearning(l.ID)
				continue
			}
			result.Errors = append(result.Errors, fmt.Sprintf("cannot write %s: %v", notePath, err))
			continue
		}

		result.Created = append(result.Created, notePath)
		state.MarkLearning(l.ID)
	}

	return result, nil
}

// checkLearningsSchemaVersion creates the schema_version table if absent, inserts
// version 1 as the baseline for pre-versioned databases, then verifies the
// recorded version does not exceed what this build supports.
func checkLearningsSchemaVersion(db *sql.DB) error {
	_, err := db.Exec(`CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)`)
	if err != nil {
		return fmt.Errorf("cannot create schema_version table: %w", err)
	}

	_, err = db.Exec(`INSERT INTO schema_version (version) SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version)`)
	if err != nil {
		return fmt.Errorf("cannot initialize schema_version: %w", err)
	}

	var version int
	if err := db.QueryRow(`SELECT version FROM schema_version`).Scan(&version); err != nil {
		return fmt.Errorf("cannot read schema_version: %w", err)
	}

	if version > maxSupportedLearningsVersion {
		return fmt.Errorf("learnings database schema version %d is not supported (max supported: %d); upgrade obsidian CLI", version, maxSupportedLearningsVersion)
	}

	return nil
}

// queryLearnings fetches learnings from the DB with optional domain/since filters.
func queryLearnings(db *sql.DB, opts LearningsIngestOptions) ([]Learning, error) {
	query := `
		SELECT id, type, content, context, domain, agent_type, tags,
		       seen_count, used_count, created_at, COALESCE(last_used_at, '')
		FROM learnings
		WHERE 1=1`
	var args []any

	if opts.Domain != "" {
		query += " AND domain = ?"
		args = append(args, opts.Domain)
	}

	if opts.Since > 0 {
		cutoff := time.Now().Add(-opts.Since).UTC().Format(time.RFC3339)
		query += " AND created_at >= ?"
		args = append(args, cutoff)
	}

	query += " ORDER BY created_at DESC"

	rows, err := db.Query(query, args...)
	if err != nil {
		return nil, fmt.Errorf("cannot query learnings: %w", err)
	}
	defer rows.Close()

	var learnings []Learning
	for rows.Next() {
		var l Learning
		if err := rows.Scan(
			&l.ID, &l.Type, &l.Content, &l.Context, &l.Domain,
			&l.AgentType, &l.Tags, &l.SeenCount, &l.UsedCount,
			&l.CreatedAt, &l.LastUsed,
		); err != nil {
			return nil, fmt.Errorf("cannot scan learning: %w", err)
		}
		learnings = append(learnings, l)
	}

	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("error reading learnings: %w", err)
	}

	return learnings, nil
}

// learningNotePath returns the vault-relative path for a learning note.
// Format: Learnings/{domain}/{type}-{id}.md
func learningNotePath(l Learning) string {
	// Sanitize the type for use in filename
	lType := strings.ReplaceAll(l.Type, "/", "-")
	lType = strings.ReplaceAll(lType, " ", "-")
	if lType == "" {
		lType = "general"
	}

	// Use a short ID (last segment after underscore for readability, or full if short)
	id := l.ID
	if len(id) > 20 {
		// Use last 16 chars
		id = id[len(id)-16:]
	}

	filename := fmt.Sprintf("%s-%s.md", lType, id)
	return filepath.Join("Learnings", l.Domain, filename)
}

// buildLearningNote generates markdown content for a learning note.
func buildLearningNote(l Learning) string {
	var b strings.Builder

	// Parse created_at for display
	date := l.CreatedAt
	if t, err := time.Parse(time.RFC3339, l.CreatedAt); err == nil {
		date = t.Format("2006-01-02")
	}

	// Build tags
	tags := []string{"learning", l.Domain}
	if l.Type != "" {
		tags = append(tags, l.Type)
	}
	if l.AgentType != "" {
		tags = append(tags, l.AgentType)
	}
	// Parse stored tags (comma-separated)
	if l.Tags != "" {
		for _, t := range strings.Split(l.Tags, ",") {
			t = strings.TrimSpace(t)
			if t != "" {
				tags = append(tags, t)
			}
		}
	}

	// Frontmatter
	b.WriteString("---\n")
	b.WriteString("type: learning\n")
	fmt.Fprintf(&b, "domain: %s\n", l.Domain)
	fmt.Fprintf(&b, "learning-type: %s\n", l.Type)
	if l.AgentType != "" {
		fmt.Fprintf(&b, "agent-type: %s\n", l.AgentType)
	}
	fmt.Fprintf(&b, "created: %s\n", date)
	fmt.Fprintf(&b, "seen-count: %d\n", l.SeenCount)
	fmt.Fprintf(&b, "used-count: %d\n", l.UsedCount)
	if l.LastUsed != "" {
		if t, err := time.Parse(time.RFC3339, l.LastUsed); err == nil {
			fmt.Fprintf(&b, "last-used: %s\n", t.Format("2006-01-02"))
		}
	}
	fmt.Fprintf(&b, "tags: [%s]\n", strings.Join(tags, ", "))
	fmt.Fprintf(&b, "ingested: %s\n", time.Now().Format("2006-01-02"))
	b.WriteString("---\n\n")

	// Title derived from type + domain
	fmt.Fprintf(&b, "# %s: %s\n\n", capitalize(l.Domain), capitalize(l.Type))

	// Content
	b.WriteString(strings.TrimSpace(l.Content))
	b.WriteString("\n")

	// Context if present
	if strings.TrimSpace(l.Context) != "" {
		b.WriteString("\n## Context\n\n")
		b.WriteString(strings.TrimSpace(l.Context))
		b.WriteString("\n")
	}

	// Metadata
	b.WriteString("\n## Metadata\n\n")
	fmt.Fprintf(&b, "- **ID**: `%s`\n", l.ID)
	fmt.Fprintf(&b, "- **Domain**: %s\n", l.Domain)
	fmt.Fprintf(&b, "- **Type**: %s\n", l.Type)
	if l.AgentType != "" {
		fmt.Fprintf(&b, "- **Agent**: %s\n", l.AgentType)
	}
	fmt.Fprintf(&b, "- **Created**: %s\n", date)
	fmt.Fprintf(&b, "- **Seen**: %d times\n", l.SeenCount)
	fmt.Fprintf(&b, "- **Used**: %d times\n", l.UsedCount)

	return b.String()
}

// capitalize uppercases the first letter of a string.
func capitalize(s string) string {
	if s == "" {
		return s
	}
	return strings.ToUpper(s[:1]) + s[1:]
}
