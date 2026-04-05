package preflight

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	_ "modernc.org/sqlite"
)

const trackerBlockTitle = "Open P0/P1 Issues"

func init() {
	Register(&trackerSection{})
}

// trackerSection surfaces open P0/P1 tracker issues from ~/.alluka/tracker.db.
type trackerSection struct{}

func (t *trackerSection) Name() string     { return "tracker" }
func (t *trackerSection) Priority() int    { return 20 }

func (t *trackerSection) Fetch(ctx context.Context) (Block, error) {
	dbPath := trackerDBPath()

	if _, err := os.Stat(dbPath); errors.Is(err, os.ErrNotExist) {
		// Missing store is normal on fresh installs — return empty block.
		return Block{Title: trackerBlockTitle}, nil
	}

	db, err := sql.Open("sqlite", dbPath+"?mode=ro")
	if err != nil {
		return Block{}, fmt.Errorf("opening tracker db: %w", err)
	}
	defer db.Close()

	rows, err := db.QueryContext(ctx, `
		SELECT id, title, COALESCE(assignee, '') AS assignee
		FROM issues
		WHERE status = 'open'
		  AND priority IN ('P0', 'P1')
		ORDER BY priority ASC, created_at ASC
		LIMIT 10
	`)
	if err != nil {
		return Block{}, fmt.Errorf("querying tracker issues: %w", err)
	}
	defer rows.Close()

	var sb strings.Builder
	for rows.Next() {
		var id, title, assignee string
		if err := rows.Scan(&id, &title, &assignee); err != nil {
			return Block{}, fmt.Errorf("scanning tracker row: %w", err)
		}
		if assignee != "" {
			fmt.Fprintf(&sb, "- [%s] %s (@%s)\n", id, title, assignee)
		} else {
			fmt.Fprintf(&sb, "- [%s] %s\n", id, title)
		}
	}
	if err := rows.Err(); err != nil {
		return Block{}, fmt.Errorf("iterating tracker rows: %w", err)
	}

	return Block{
		Title: trackerBlockTitle,
		Body:  strings.TrimRight(sb.String(), "\n"),
	}, nil
}

// trackerDBPath returns the path to tracker.db.
// Resolution order:
//  1. TRACKER_DB env var (test override)
//  2. $ALLUKA_HOME/tracker.db
//  3. ~/.alluka/tracker.db (default)
func trackerDBPath() string {
	if v := os.Getenv("TRACKER_DB"); v != "" {
		return v
	}
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return filepath.Join(v, "tracker.db")
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "tracker.db")
}
