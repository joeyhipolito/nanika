// Package browser extracts cookies from browser cookie stores on macOS.
package browser

import "fmt"

// Supported cookie names, tried in order.
var cookieNames = []string{"substack.sid", "connect.sid"}

// ExtractCookie finds a Substack session cookie from the specified browser.
// browserName: "chrome" or "firefox"
// Returns the raw cookie value (without name= prefix) or error.
func ExtractCookie(browserName string) (string, error) {
	switch browserName {
	case "chrome":
		return extractChrome()
	case "firefox":
		return extractFirefox()
	default:
		return "", fmt.Errorf("unsupported browser: %s (supported: chrome, firefox)", browserName)
	}
}
