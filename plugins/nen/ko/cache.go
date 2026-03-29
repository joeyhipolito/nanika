package ko

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

// Cache stores LLM responses in SQLite, keyed by sha256(model:prompt).
type Cache struct {
	db  *sql.DB
	ttl time.Duration
}

// DefaultCacheDBPath returns ~/.alluka/ko-cache.db.
func DefaultCacheDBPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "ko-cache.db")
}

// CacheKey returns the sha256 hex key for a given model and prompt.
func CacheKey(model, prompt string) string {
	h := sha256.Sum256([]byte(model + ":" + prompt))
	return fmt.Sprintf("%x", h)
}

// OpenCache opens (or creates) the cache database at path with the given TTL.
// A leading ~/ is expanded to the user home directory.
func OpenCache(path string, ttl time.Duration) (*Cache, error) {
	if strings.HasPrefix(path, "~/") {
		home, _ := os.UserHomeDir()
		path = filepath.Join(home, path[2:])
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return nil, fmt.Errorf("create cache dir: %w", err)
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open cache db: %w", err)
	}
	if err := migrateCacheDB(db); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate cache: %w", err)
	}
	return &Cache{db: db, ttl: ttl}, nil
}

func migrateCacheDB(db *sql.DB) error {
	_, err := db.Exec(`
		CREATE TABLE IF NOT EXISTS cache_entries (
			key        TEXT PRIMARY KEY,
			model      TEXT NOT NULL DEFAULT '',
			response   TEXT NOT NULL DEFAULT '',
			created_at TEXT NOT NULL,
			expires_at TEXT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_cache_expires ON cache_entries(expires_at);
	`)
	return err
}

// Get returns the cached response for key if it exists and has not expired.
// Returns ("", false, nil) on a miss.
func (c *Cache) Get(ctx context.Context, key string) (string, bool, error) {
	now := time.Now().UTC().Format(time.RFC3339)
	var response string
	err := c.db.QueryRowContext(ctx,
		`SELECT response FROM cache_entries WHERE key = ? AND expires_at > ?`,
		key, now,
	).Scan(&response)
	if err == sql.ErrNoRows {
		return "", false, nil
	}
	if err != nil {
		return "", false, fmt.Errorf("cache get: %w", err)
	}
	return response, true, nil
}

// Set stores response in the cache under key, expiring after the configured TTL.
func (c *Cache) Set(ctx context.Context, key, model, response string) error {
	now := time.Now().UTC()
	_, err := c.db.ExecContext(ctx, `
		INSERT INTO cache_entries (key, model, response, created_at, expires_at)
		VALUES (?, ?, ?, ?, ?)
		ON CONFLICT(key) DO UPDATE SET
			model      = excluded.model,
			response   = excluded.response,
			created_at = excluded.created_at,
			expires_at = excluded.expires_at`,
		key, model, response,
		now.Format(time.RFC3339),
		now.Add(c.ttl).Format(time.RFC3339),
	)
	if err != nil {
		return fmt.Errorf("cache set: %w", err)
	}
	return nil
}

// Close closes the underlying database connection.
func (c *Cache) Close() error {
	return c.db.Close()
}
