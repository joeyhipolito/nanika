package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/browser"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// ConfigureCmd handles the configure command and its sub-commands.
func ConfigureCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 && args[0] == "show" {
		return configureShowCmd(jsonOutput)
	}

	// Parse --from-browser flag
	for i := 0; i < len(args); i++ {
		if args[i] == "--from-browser" {
			if i+1 >= len(args) {
				return fmt.Errorf("--from-browser requires a browser name (chrome, firefox)")
			}
			return configureFromBrowser(args[i+1])
		}
	}

	return configureInteractive()
}

func configureInteractive() error {
	reader := bufio.NewReader(os.Stdin)

	// Check existing config
	if config.Exists() {
		fmt.Print("Configuration already exists. Overwrite? [y/N]: ")
		answer, _ := reader.ReadString('\n')
		answer = strings.TrimSpace(strings.ToLower(answer))
		if answer != "y" && answer != "yes" {
			fmt.Println("Configuration unchanged.")
			return nil
		}
	}

	fmt.Println("Substack CLI Configuration")
	fmt.Println("==========================")
	fmt.Println()
	fmt.Println("You'll need your substack.sid cookie from Substack.")
	fmt.Println("To get it: open Substack in your browser → DevTools → Application → Cookies → copy substack.sid value")
	fmt.Println()

	// Prompt for cookie
	fmt.Print("substack.sid cookie value: ")
	cookie, _ := reader.ReadString('\n')
	cookie = strings.TrimSpace(cookie)
	if cookie == "" {
		return fmt.Errorf("cookie is required")
	}
	cookie = normalizeCookie(cookie)

	// Prompt for publication URL
	fmt.Print("Publication URL (e.g., https://yourname.substack.com): ")
	pubURL, _ := reader.ReadString('\n')
	pubURL = strings.TrimSpace(pubURL)
	if pubURL == "" {
		return fmt.Errorf("publication URL is required")
	}

	// Extract subdomain from URL
	subdomain := extractSubdomain(pubURL)
	if subdomain == "" {
		return fmt.Errorf("could not extract subdomain from URL: %s", pubURL)
	}

	fmt.Printf("Extracted subdomain: %s\n", subdomain)
	fmt.Println()

	// Verify auth
	fmt.Println("Verifying authentication...")
	client := api.NewClient(subdomain, cookie)
	user, err := client.GetProfile()
	if err != nil {
		return fmt.Errorf("authentication failed: %w", err)
	}
	fmt.Printf("Authenticated as: %s (%s)\n", user.Name, user.Email)
	fmt.Println()

	// Save config
	cfg := &config.Config{
		Cookie:         cookie,
		PublicationURL: pubURL,
		Subdomain:      subdomain,
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}

	p, _ := config.Path()
	fmt.Printf("Configuration saved to %s\n", p)
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  substack doctor    — verify everything is working")
	fmt.Println("  substack publish   — cross-post a blog post")
	fmt.Println("  substack drafts    — list your drafts")

	return nil
}

func configureFromBrowser(browserName string) error {
	reader := bufio.NewReader(os.Stdin)

	// Check existing config
	if config.Exists() {
		fmt.Print("Configuration already exists. Overwrite? [y/N]: ")
		answer, _ := reader.ReadString('\n')
		answer = strings.TrimSpace(strings.ToLower(answer))
		if answer != "y" && answer != "yes" {
			fmt.Println("Configuration unchanged.")
			return nil
		}
	}

	fmt.Println("Substack CLI Configuration (browser extraction)")
	fmt.Println("================================================")
	fmt.Println()
	fmt.Printf("Extracting cookie from %s...\n", browserName)

	// Extract cookie from browser
	rawValue, err := browser.ExtractCookie(browserName)
	if err != nil {
		return fmt.Errorf("browser cookie extraction failed: %w\n\nFallback: run 'substack configure' to paste the cookie manually", err)
	}

	cookie := normalizeCookie(rawValue)
	fmt.Printf("Cookie found: %s\n", maskCookie(cookie))
	fmt.Println()

	// Still need publication URL
	fmt.Print("Publication URL (e.g., https://yourname.substack.com): ")
	pubURL, _ := reader.ReadString('\n')
	pubURL = strings.TrimSpace(pubURL)
	if pubURL == "" {
		return fmt.Errorf("publication URL is required")
	}

	// Extract subdomain from URL
	subdomain := extractSubdomain(pubURL)
	if subdomain == "" {
		return fmt.Errorf("could not extract subdomain from URL: %s", pubURL)
	}

	fmt.Printf("Extracted subdomain: %s\n", subdomain)
	fmt.Println()

	// Verify auth
	fmt.Println("Verifying authentication...")
	client := api.NewClient(subdomain, cookie)
	user, err := client.GetProfile()
	if err != nil {
		return fmt.Errorf("authentication failed: %w\n\nThe cookie was extracted but may be expired. Try logging into Substack in your browser first", err)
	}
	fmt.Printf("Authenticated as: %s (%s)\n", user.Name, user.Email)
	fmt.Println()

	// Save config
	cfg := &config.Config{
		Cookie:         cookie,
		PublicationURL: pubURL,
		Subdomain:      subdomain,
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}

	p, _ := config.Path()
	fmt.Printf("Configuration saved to %s\n", p)
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  substack doctor    — verify everything is working")
	fmt.Println("  substack publish   — cross-post a blog post")
	fmt.Println("  substack drafts    — list your drafts")

	return nil
}

func configureShowCmd(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	if !config.Exists() {
		return fmt.Errorf("no configuration found. Run 'substack configure' first")
	}

	if jsonOutput {
		type showOutput struct {
			Cookie         string `json:"cookie"`
			PublicationURL string `json:"publication_url"`
			Subdomain      string `json:"subdomain"`
			SiteURL        string `json:"site_url,omitempty"`
			Path           string `json:"path"`
		}
		p, _ := config.Path()
		out := showOutput{
			Cookie:         maskCookie(cfg.Cookie),
			PublicationURL: cfg.PublicationURL,
			Subdomain:      cfg.Subdomain,
			SiteURL:        cfg.SiteURL,
			Path:           p,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	p, _ := config.Path()
	fmt.Printf("Config path:      %s\n", p)
	fmt.Printf("Cookie:           %s\n", maskCookie(cfg.Cookie))
	fmt.Printf("Publication URL:  %s\n", cfg.PublicationURL)
	fmt.Printf("Subdomain:        %s\n", cfg.Subdomain)
	if cfg.SiteURL != "" {
		fmt.Printf("Site URL:         %s\n", cfg.SiteURL)
	}

	return nil
}

// extractSubdomain extracts the subdomain from a Substack URL.
func extractSubdomain(pubURL string) string {
	url := strings.TrimPrefix(pubURL, "https://")
	url = strings.TrimPrefix(url, "http://")
	url = strings.TrimSuffix(url, "/")

	parts := strings.Split(url, ".")
	if len(parts) >= 3 && parts[len(parts)-2] == "substack" && parts[len(parts)-1] == "com" {
		return strings.Join(parts[:len(parts)-2], ".")
	}
	return ""
}

// normalizeCookie ensures the cookie value has the substack.sid= prefix.
// Accepts: raw value, "substack.sid=<value>", or "connect.sid=<value>".
func normalizeCookie(cookie string) string {
	if strings.HasPrefix(cookie, "substack.sid=") {
		return cookie
	}
	if strings.HasPrefix(cookie, "connect.sid=") {
		// Replace connect.sid with substack.sid
		return "substack.sid=" + strings.TrimPrefix(cookie, "connect.sid=")
	}
	// Raw value — prepend the cookie name
	return "substack.sid=" + cookie
}

// maskCookie shows only the first 8 and last 4 characters of the cookie.
func maskCookie(cookie string) string {
	if len(cookie) <= 16 {
		return "****"
	}
	return cookie[:8] + "..." + cookie[len(cookie)-4:]
}
