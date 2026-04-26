package preflight

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	_ "modernc.org/sqlite"
)

const trackerBlockTitle = "Open P0/P1 Issues"

func init() {
	Register(&trackerSection{})
}

// trackerSection surfaces open P0/P1 tracker issues from ~/.alluka/tracker.db.
type trackerSection struct{}

func (t *trackerSection) Name() string  { return "tracker" }
func (t *trackerSection) Priority() int { return 20 }

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

	limit := trackerLimit()

	// G6: use seq_id when present so output matches CLI format (TRK-<n>);
	// legacy rows without seq_id fall back to their hash id.
	rows, err := db.QueryContext(ctx, `
		SELECT
			CASE WHEN seq_id IS NOT NULL THEN 'TRK-' || seq_id ELSE id END AS display_id,
			title,
			COALESCE(assignee, '') AS assignee
		FROM issues
		WHERE status = 'open'
		  AND priority IN ('P0', 'P1')
		ORDER BY priority ASC, created_at ASC
		LIMIT ?
	`, limit)
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

	var total int
	if err := db.QueryRowContext(ctx, `
		SELECT count(*) FROM issues
		WHERE status = 'open' AND priority IN ('P0', 'P1')
	`).Scan(&total); err != nil {
		return Block{}, fmt.Errorf("counting tracker issues: %w", err)
	}

	body := strings.TrimRight(sb.String(), "\n")
	if total > limit {
		body += fmt.Sprintf("\n- _(showing %d of %d; set NANIKA_PREFLIGHT_TRACKER_LIMIT to see more)_", limit, total)
	}

	return Block{
		Title: trackerBlockTitle,
		Body:  body,
	}, nil
}

// trackerLimit returns the maximum number of issues to fetch.
// Read NANIKA_PREFLIGHT_TRACKER_LIMIT env var; default to 25.
func trackerLimit() int {
	if v := os.Getenv("NANIKA_PREFLIGHT_TRACKER_LIMIT"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			return n
		}
	}
	return 25
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
