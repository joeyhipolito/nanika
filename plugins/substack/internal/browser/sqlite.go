package browser

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// queryDB copies the SQLite database to a temp directory (to avoid browser
// write locks) and executes the given SQL query using the system sqlite3 CLI.
func queryDB(dbPath, query string) (string, error) {
	// Verify sqlite3 is available
	if _, err := exec.LookPath("sqlite3"); err != nil {
		return "", fmt.Errorf("sqlite3 not found — install Xcode Command Line Tools: xcode-select --install")
	}

	// Verify source DB exists
	if _, err := os.Stat(dbPath); err != nil {
		return "", fmt.Errorf("database not found: %s", dbPath)
	}

	// Copy DB to temp dir to avoid browser write locks
	tmpDir, err := os.MkdirTemp("", "substack-cookies-*")
	if err != nil {
		return "", fmt.Errorf("creating temp directory: %w", err)
	}
	defer os.RemoveAll(tmpDir)

	tmpDB := filepath.Join(tmpDir, "cookies.db")
	if err := copyFile(dbPath, tmpDB); err != nil {
		return "", fmt.Errorf("copying database: %w", err)
	}

	// Also copy WAL and SHM files if they exist (needed for WAL mode databases)
	for _, suffix := range []string{"-wal", "-shm"} {
		src := dbPath + suffix
		if _, err := os.Stat(src); err == nil {
			_ = copyFile(src, tmpDB+suffix)
		}
	}

	// Execute query
	out, err := exec.Command("sqlite3", tmpDB, query).Output()
	if err != nil {
		return "", fmt.Errorf("sqlite3 query failed: %w", err)
	}

	return strings.TrimSpace(string(out)), nil
}

// copyFile copies a file from src to dst.
func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()

	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	defer out.Close()

	if _, err := io.Copy(out, in); err != nil {
		return err
	}
	return out.Close()
}
