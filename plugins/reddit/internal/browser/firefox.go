package browser

import (
	"fmt"
	"os"
	"path/filepath"
)

// extractFirefox extracts Reddit cookies from Firefox on macOS.
// Firefox stores cookies in plaintext (no decryption needed).
func extractFirefox() (*CookieResult, error) {
	dbPath, err := findFirefoxDB()
	if err != nil {
		return nil, fmt.Errorf("finding Firefox cookies: %w", err)
	}

	// Extract reddit_session
	sessionQuery := `SELECT value FROM moz_cookies WHERE host LIKE '%.reddit.com' AND name='reddit_session' ORDER BY lastAccessed DESC LIMIT 1`
	redditSession, err := queryDB(dbPath, sessionQuery)
	if err != nil {
		return nil, fmt.Errorf("querying Firefox cookies for reddit_session: %w", err)
	}
	if redditSession == "" {
		return nil, fmt.Errorf("no reddit_session cookie found in Firefox — are you logged in to reddit.com?")
	}

	// Extract csrf_token
	csrfQuery := `SELECT value FROM moz_cookies WHERE host LIKE '%.reddit.com' AND name='csrf_token' ORDER BY lastAccessed DESC LIMIT 1`
	csrfToken, err := queryDB(dbPath, csrfQuery)
	if err != nil {
		return nil, fmt.Errorf("querying Firefox cookies for csrf_token: %w", err)
	}
	if csrfToken == "" {
		return nil, fmt.Errorf("no csrf_token cookie found in Firefox — are you logged in to reddit.com?")
	}

	return &CookieResult{RedditSession: redditSession, CSRFToken: csrfToken}, nil
}

// findFirefoxDB locates the Firefox cookie database on macOS.
func findFirefoxDB() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}

	profilesDir := filepath.Join(home, "Library", "Application Support", "Firefox", "Profiles")
	if _, err := os.Stat(profilesDir); err != nil {
		return "", fmt.Errorf("Firefox profiles not found at %s", profilesDir)
	}

	matches, err := filepath.Glob(filepath.Join(profilesDir, "*", "cookies.sqlite"))
	if err != nil {
		return "", fmt.Errorf("searching Firefox profiles: %w", err)
	}
	if len(matches) == 0 {
		return "", fmt.Errorf("no Firefox cookie databases found — is Firefox installed?")
	}

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
