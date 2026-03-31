package browser

import (
	"fmt"
	"os"
	"path/filepath"
)

// extractFirefox extracts a Substack cookie from Firefox on macOS.
// Firefox stores cookies in plaintext (no decryption needed).
func extractFirefox() (string, error) {
	dbPath, err := findFirefoxDB()
	if err != nil {
		return "", fmt.Errorf("finding Firefox cookies: %w", err)
	}

	// Query for cookie value (plaintext, no decryption needed)
	query := fmt.Sprintf(
		`SELECT value FROM moz_cookies WHERE host LIKE '%%substack.com' AND (name='substack.sid' OR name='connect.sid') ORDER BY lastAccessed DESC LIMIT 1`,
	)
	value, err := queryDB(dbPath, query)
	if err != nil {
		return "", fmt.Errorf("querying Firefox cookies: %w", err)
	}
	if value == "" {
		return "", fmt.Errorf("no Substack cookie found in Firefox — are you logged in to substack.com?")
	}

	return value, nil
}

// findFirefoxDB locates the Firefox cookie database on macOS.
// Scans all profiles and picks the most recently modified cookies.sqlite.
func findFirefoxDB() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}

	profilesDir := filepath.Join(home, "Library", "Application Support", "Firefox", "Profiles")
	if _, err := os.Stat(profilesDir); err != nil {
		return "", fmt.Errorf("Firefox profiles not found at %s", profilesDir)
	}

	// Glob for cookies.sqlite in all profiles
	matches, err := filepath.Glob(filepath.Join(profilesDir, "*", "cookies.sqlite"))
	if err != nil {
		return "", fmt.Errorf("searching Firefox profiles: %w", err)
	}
	if len(matches) == 0 {
		return "", fmt.Errorf("no Firefox cookie databases found — is Firefox installed?")
	}

	// Pick the most recently modified
	var bestPath string
	var bestTime int64
	for _, m := range matches {
		info, err := os.Stat(m)
		if err != nil {
			continue
		}
		if t := info.ModTime().UnixNano(); t > bestTime {
			bestTime = t
			bestPath = m
		}
	}

	if bestPath == "" {
		return "", fmt.Errorf("could not read any Firefox cookie database")
	}

	return bestPath, nil
}
