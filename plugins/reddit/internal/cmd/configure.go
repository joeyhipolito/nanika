package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/browser"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
)

type configShowOutput struct {
	RedditSession string `json:"reddit_session"`
	CSRFToken     string `json:"csrf_token"`
	Username      string `json:"username"`
	Path          string `json:"path"`
}

// ConfigureCmd handles the configure command and its sub-commands.
func ConfigureCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 {
		switch args[0] {
		case "show":
			return configureShowCmd(jsonOutput)
		case "cookies":
			browserName := "chrome"
			for i := 1; i < len(args); i++ {
				if args[i] == "--from-browser" && i+1 < len(args) {
					browserName = args[i+1]
					i++
				}
			}
			return configureCookiesCmd(browserName)
		}
	}
	// Default: show help since Reddit is cookie-only
	fmt.Println("Reddit CLI — Configuration")
	fmt.Println("==========================")
	fmt.Println()
	fmt.Println("Reddit CLI uses browser cookies for authentication.")
	fmt.Println()
	fmt.Println("Commands:")
	fmt.Println("  reddit configure cookies              Extract cookies from Chrome")
	fmt.Println("  reddit configure cookies --from-browser firefox  Extract from Firefox")
	fmt.Println("  reddit configure show                 Show current config")
	fmt.Println("  reddit configure show --json           Show config as JSON")
	return nil
}

func configureCookiesCmd(browserName string) error {
	fmt.Println("Reddit CLI — Cookie Configuration")
	fmt.Println("==================================")
	fmt.Println()
	fmt.Printf("Extracting cookies from %s...\n", browserName)

	result, err := browser.ExtractCookies(browserName)
	if err != nil {
		return fmt.Errorf("browser cookie extraction failed: %w\n\nFallback: log in to reddit.com in your browser and try again", err)
	}

	fmt.Printf("  reddit_session:  %s\n", maskValue(result.RedditSession))
	fmt.Printf("  csrf_token:      %s\n", maskValue(result.CSRFToken))
	fmt.Println()

	// Test auth and get username
	fmt.Println("Verifying authentication...")
	client := api.NewRedditClient(result.RedditSession, result.CSRFToken)
	username, err := client.TestAuth()
	if err != nil {
		fmt.Printf("  Warning: could not verify auth: %v\n", err)
		fmt.Println("  Cookies saved anyway — they may still work for some operations.")
		username = ""
	} else {
		fmt.Printf("  Authenticated as: %s\n", username)
	}
	fmt.Println()

	cfg := &config.Config{
		RedditSession: result.RedditSession,
		CSRFToken:     result.CSRFToken,
		Username:      username,
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}

	p, _ := config.Path()
	fmt.Printf("Configuration saved to %s\n", p)
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  reddit doctor  — verify everything is working")

	return nil
}

func configureShowCmd(jsonOutput bool) error {
	if !config.Exists() {
		return fmt.Errorf("not configured. Run 'reddit configure cookies' first")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	p, _ := config.Path()

	if jsonOutput {
		out := configShowOutput{
			RedditSession: maskValue(cfg.RedditSession),
			CSRFToken:     maskValue(cfg.CSRFToken),
			Username:      cfg.Username,
			Path:          p,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Println("Reddit Configuration")
	fmt.Println("====================")
	fmt.Println()
	fmt.Println("Cookies:")
	fmt.Printf("  reddit_session:  %s\n", maskValue(cfg.RedditSession))
	fmt.Printf("  csrf_token:      %s\n", maskValue(cfg.CSRFToken))
	fmt.Printf("  username:        %s\n", cfg.Username)
	fmt.Println()
	fmt.Printf("Config path: %s\n", p)

	return nil
}
