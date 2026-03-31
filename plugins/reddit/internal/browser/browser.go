// Package browser extracts Reddit cookies from browser cookie stores on macOS.
package browser

import "fmt"

// CookieResult holds extracted Reddit cookies.
type CookieResult struct {
	RedditSession string // reddit_session cookie value
	CSRFToken     string // csrf_token cookie value
}

// ExtractCookies finds Reddit session cookies from the specified browser.
// browserName: "chrome" or "firefox"
func ExtractCookies(browserName string) (*CookieResult, error) {
	switch browserName {
	case "chrome":
		return extractChrome()
	case "firefox":
		return extractFirefox()
	default:
		return nil, fmt.Errorf("unsupported browser: %s (supported: chrome, firefox)", browserName)
	}
}
