package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/browser"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
)

type configShowOutput struct {
	ClientID       string `json:"client_id"`
	AccessToken    string `json:"access_token"`
	TokenExpiry    string `json:"token_expiry"`
	PersonURN      string `json:"person_urn"`
	ChromeDebugURL string `json:"chrome_debug_url"`
	Path           string `json:"path"`
}

// ConfigureCmd handles the configure command and its sub-commands.
func ConfigureCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 {
		switch args[0] {
		case "show":
			return configureShowCmd(jsonOutput)
		case "chrome":
			return configureChromeCmd()
		}
	}
	return configureOAuthCmd()
}

func configureOAuthCmd() error {
	reader := bufio.NewReader(os.Stdin)

	existing, _ := config.Load()

	fmt.Println("LinkedIn CLI — OAuth Configuration")
	fmt.Println("===================================")
	fmt.Println()
	fmt.Println("You'll need a LinkedIn app with the 'Sign In with LinkedIn using OpenID Connect'")
	fmt.Println("and 'Share on LinkedIn' products enabled.")
	fmt.Println()
	fmt.Println("Create one at: https://www.linkedin.com/developers/apps")
	fmt.Println()

	fmt.Print("Client ID: ")
	clientID, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("reading input: %w", err)
	}
	clientID = strings.TrimSpace(clientID)
	if clientID == "" {
		return fmt.Errorf("client ID is required")
	}

	fmt.Print("Client Secret: ")
	clientSecret, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("reading input: %w", err)
	}
	clientSecret = strings.TrimSpace(clientSecret)
	if clientSecret == "" {
		return fmt.Errorf("client secret is required")
	}

	fmt.Println()

	result, err := api.RunOAuthFlow(clientID, clientSecret)
	if err != nil {
		return fmt.Errorf("OAuth flow failed: %w", err)
	}

	fmt.Printf("\nAuthenticated as: %s (%s)\n", result.Name, result.Email)
	fmt.Printf("Person URN: %s\n", result.PersonURN)

	expiry := time.Now().Add(time.Duration(result.ExpiresIn) * time.Second)

	cfg := &config.Config{
		ClientID:     clientID,
		ClientSecret: clientSecret,
		AccessToken:  result.AccessToken,
		TokenExpiry:  expiry.Format(time.RFC3339),
		PersonURN:    result.PersonURN,
	}

	// Preserve existing chrome debug URL
	if existing != nil {
		cfg.ChromeDebugURL = existing.ChromeDebugURL
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}

	p, _ := config.Path()
	fmt.Printf("\nConfiguration saved to %s\n", p)
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  linkedin chrome   — launch Chrome with remote debugging for feed reading")
	fmt.Println("  linkedin doctor   — verify everything is working")

	return nil
}

func configureChromeCmd() error {
	existing, _ := config.Load()
	if existing == nil {
		existing = &config.Config{}
	}

	cdp := browser.NewCDPClient(existing.ChromeDebugURL)
	if err := cdp.TestConnection(); err != nil {
		fmt.Println("Chrome CDP connection: FAILED")
		fmt.Printf("  Error: %v\n", err)
		fmt.Println()
		fmt.Println("Launch Chrome with remote debugging:")
		fmt.Println()
		fmt.Println("  /Applications/Google Chrome.app/Contents/MacOS/Google Chrome \\")
		fmt.Println("    --remote-debugging-port=9222 \\")
		fmt.Println("    --user-data-dir=~/.chrome-linkedin")
		fmt.Println()
		fmt.Println("Then log into LinkedIn in that browser window.")
		return nil
	}

	url := existing.ChromeDebugURL
	if url == "" {
		url = browser.DefaultRemoteURL
	}
	fmt.Printf("Chrome CDP connection: OK (%s)\n", url)

	status, err := cdp.IsLoggedIn()
	if err != nil {
		fmt.Printf("LinkedIn session: could not check (%v)\n", err)
	} else if !status.LoggedIn {
		fmt.Println("LinkedIn session: NOT LOGGED IN")
		fmt.Println("  Open the Chrome window and log into linkedin.com")
	} else {
		msg := "logged in"
		if status.Name != "" {
			msg = fmt.Sprintf("logged in as %s", status.Name)
		}
		fmt.Printf("LinkedIn session: %s\n", msg)
	}

	return nil
}

func configureShowCmd(jsonOutput bool) error {
	if !config.Exists() {
		return fmt.Errorf("not configured. Run 'linkedin configure' first")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	p, _ := config.Path()

	if jsonOutput {
		chromeURL := cfg.ChromeDebugURL
		if chromeURL == "" {
			chromeURL = browser.DefaultRemoteURL
		}
		out := configShowOutput{
			ClientID:       cfg.ClientID,
			AccessToken:    maskValue(cfg.AccessToken),
			TokenExpiry:    cfg.TokenExpiry,
			PersonURN:      cfg.PersonURN,
			ChromeDebugURL: chromeURL,
			Path:           p,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	chromeURL := cfg.ChromeDebugURL
	if chromeURL == "" {
		chromeURL = browser.DefaultRemoteURL + " (default)"
	}

	fmt.Println("LinkedIn Configuration")
	fmt.Println("======================")
	fmt.Println()
	fmt.Println("OAuth (Official API):")
	fmt.Printf("  Client ID:     %s\n", cfg.ClientID)
	fmt.Printf("  Access Token:  %s\n", maskValue(cfg.AccessToken))
	fmt.Printf("  Token Expiry:  %s\n", cfg.TokenExpiry)
	fmt.Printf("  Person URN:    %s\n", cfg.PersonURN)
	fmt.Println()
	fmt.Println("Chrome CDP (Feed Reading):")
	fmt.Printf("  Debug URL:     %s\n", chromeURL)
	fmt.Println()
	fmt.Printf("Config path: %s\n", p)

	return nil
}

func maskValue(v string) string {
	if len(v) < 12 {
		if v == "" {
			return "(not set)"
		}
		return "****"
	}
	return v[:8] + "..." + v[len(v)-4:]
}
