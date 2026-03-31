package browser

import (
	"encoding/hex"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
)

// extractChrome extracts Reddit cookies from Chrome on macOS.
func extractChrome() (*CookieResult, error) {
	dbPath, err := findChromeDB()
	if err != nil {
		return nil, fmt.Errorf("finding Chrome cookies: %w", err)
	}

	// Get Keychain password for Chrome Safe Storage
	fmt.Println("  Requesting Chrome Safe Storage password from Keychain...")
	fmt.Println("  (macOS may prompt you to allow access)")
	password, err := getChromeKeychainPassword()
	if err != nil {
		return nil, fmt.Errorf("Keychain access failed: %w\n  Grant permission to your terminal app in the macOS dialog", err)
	}

	// Get meta_version for hash prefix handling
	metaVersion := 0
	metaQuery := `SELECT value FROM meta WHERE key='version'`
	metaStr, err := queryDB(dbPath, metaQuery)
	if err == nil && metaStr != "" {
		metaVersion, _ = strconv.Atoi(metaStr)
	}

	// Extract reddit_session cookie
	redditSession, err := extractChromeCookie(dbPath, "reddit_session", password, metaVersion)
	if err != nil {
		return nil, fmt.Errorf("extracting reddit_session: %w", err)
	}

	// Extract csrf_token cookie
	csrfToken, err := extractChromeCookie(dbPath, "csrf_token", password, metaVersion)
	if err != nil {
		return nil, fmt.Errorf("extracting csrf_token: %w", err)
	}

	return &CookieResult{RedditSession: redditSession, CSRFToken: csrfToken}, nil
}

func extractChromeCookie(dbPath, cookieName, password string, metaVersion int) (string, error) {
	query := fmt.Sprintf(
		`SELECT hex(encrypted_value) FROM cookies WHERE host_key LIKE '%%.reddit.com' AND name='%s' ORDER BY last_access_utc DESC LIMIT 1`,
		cookieName,
	)
	hexValue, err := queryDB(dbPath, query)
	if err != nil {
		return "", fmt.Errorf("querying Chrome cookies for %s: %w", cookieName, err)
	}
	if hexValue == "" {
		return "", fmt.Errorf("no %s cookie found in Chrome — are you logged in to reddit.com?", cookieName)
	}

	encrypted, err := hex.DecodeString(hexValue)
	if err != nil {
		return "", fmt.Errorf("decoding cookie value: %w", err)
	}

	value, err := decryptChromeValue(encrypted, password, metaVersion)
	if err != nil {
		return "", fmt.Errorf("decrypting Chrome cookie: %w", err)
	}

	return value, nil
}

// findChromeDB locates the Chrome cookie database on macOS.
func findChromeDB() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}

	chromeBase := filepath.Join(home, "Library", "Application Support", "Google", "Chrome")
	if _, err := os.Stat(chromeBase); err != nil {
		return "", fmt.Errorf("Chrome not found at %s", chromeBase)
	}

	var bestPath string
	var bestTime int64

	defaultDB := filepath.Join(chromeBase, "Default", "Cookies")
	if info, err := os.Stat(defaultDB); err == nil {
		bestPath = defaultDB
		bestTime = info.ModTime().UnixNano()
	}

	entries, err := os.ReadDir(chromeBase)
	if err != nil {
		return "", fmt.Errorf("reading Chrome directory: %w", err)
	}

	for _, e := range entries {
		if e.IsDir() && (strings.HasPrefix(e.Name(), "Profile ") || e.Name() == "Default") {
			db := filepath.Join(chromeBase, e.Name(), "Cookies")
			if info, err := os.Stat(db); err == nil {
				if t := info.ModTime().UnixNano(); t > bestTime {
					bestTime = t
					bestPath = db
				}
			}
		}
	}

	if bestPath == "" {
		return "", fmt.Errorf("Chrome cookie database not found — is Chrome installed?")
	}

	return bestPath, nil
}

// getChromeKeychainPassword retrieves the Chrome Safe Storage password from macOS Keychain.
func getChromeKeychainPassword() (string, error) {
	out, err := exec.Command("security", "find-generic-password", "-w", "-a", "Chrome", "-s", "Chrome Safe Storage").Output()
	if err != nil {
		return "", fmt.Errorf("security command failed: %w", err)
	}
	return strings.TrimSpace(string(out)), nil
}
