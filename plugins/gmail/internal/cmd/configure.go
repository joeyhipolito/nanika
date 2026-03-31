package cmd

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"google.golang.org/api/gmail/v1"
	"google.golang.org/api/option"

	"github.com/joeyhipolito/nanika-gmail/internal/auth"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// ConfigureCmd handles "gmail configure <alias>".
// It ensures client credentials exist, runs OAuth2, fetches the user's email,
// and registers the account.
func ConfigureCmd(alias string) error {
	reader := bufio.NewReader(os.Stdin)

	// 1. If no config exists, prompt for client credentials
	if !config.Exists() {
		fmt.Println("Gmail CLI Configuration")
		fmt.Println("=======================")
		fmt.Println()
		fmt.Println("No configuration found. Let's set up OAuth2 credentials.")
		fmt.Println("Get yours from: https://console.cloud.google.com/apis/credentials")
		fmt.Println()

		fmt.Print("Client ID: ")
		clientID, _ := reader.ReadString('\n')
		clientID = strings.TrimSpace(clientID)
		if clientID == "" {
			return fmt.Errorf("client_id is required")
		}

		fmt.Print("Client Secret: ")
		clientSecret, _ := reader.ReadString('\n')
		clientSecret = strings.TrimSpace(clientSecret)
		if clientSecret == "" {
			return fmt.Errorf("client_secret is required")
		}

		cfg := &config.Config{
			ClientID:     clientID,
			ClientSecret: clientSecret,
		}
		if err := config.Save(cfg); err != nil {
			return fmt.Errorf("failed to save configuration: %w", err)
		}
		fmt.Printf("Configuration saved to %s\n\n", config.Path())
	}

	// 2. Load config
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	// 3. Validate alias
	if alias == "" {
		fmt.Println("Usage: gmail configure <alias>")
		fmt.Println()
		fmt.Println("Example:")
		fmt.Println("  gmail configure personal")
		fmt.Println("  gmail configure work")
		return nil
	}

	// 4. Run OAuth2 flow
	fmt.Printf("Authorizing account %q...\n\n", alias)
	oauthCfg := auth.OAuthConfig(cfg.ClientID, cfg.ClientSecret)
	token, err := auth.Authorize(oauthCfg)
	if err != nil {
		return fmt.Errorf("authorization failed: %w", err)
	}

	// Save the token
	tokenPath := config.TokenPath(alias)
	if err := auth.SaveToken(tokenPath, token); err != nil {
		return fmt.Errorf("failed to save token: %w", err)
	}

	// 5. Fetch the user's email address
	ts := oauthCfg.TokenSource(context.Background(), token)
	svc, err := gmail.NewService(context.Background(), option.WithTokenSource(ts))
	if err != nil {
		return fmt.Errorf("failed to create Gmail service: %w", err)
	}

	profile, err := svc.Users.GetProfile("me").Do()
	if err != nil {
		return fmt.Errorf("failed to fetch Gmail profile: %w", err)
	}

	// 6. Register account (ignore "already exists" for re-auth)
	if err := config.AddAccount(alias, profile.EmailAddress); err != nil {
		if !strings.Contains(err.Error(), "already exists") {
			return fmt.Errorf("failed to register account: %w", err)
		}
	}

	// 7. Print success
	fmt.Println()
	fmt.Printf("Account %q configured successfully!\n", alias)
	fmt.Printf("  Email: %s\n", profile.EmailAddress)
	fmt.Printf("  Token: %s\n", tokenPath)
	fmt.Println()
	fmt.Println("Test your setup:")
	fmt.Println("  gmail doctor")
	fmt.Println("  gmail accounts")

	return nil
}

// ConfigureShowCmd handles "gmail configure show [--json]".
// Shows client_id (masked), client_secret (masked), and accounts list.
func ConfigureShowCmd(jsonOutput bool) error {
	if !config.Exists() {
		fmt.Println("No configuration file found.")
		fmt.Println("Run 'gmail configure <alias>' to set up.")
		return nil
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	accounts, err := config.LoadAccounts()
	if err != nil {
		return fmt.Errorf("failed to load accounts: %w", err)
	}

	maskedID := maskString(cfg.ClientID)
	maskedSecret := maskString(cfg.ClientSecret)

	if jsonOutput {
		type accountJSON struct {
			Alias   string `json:"alias"`
			Email   string `json:"email"`
			AddedAt string `json:"added_at"`
		}

		accts := make([]accountJSON, len(accounts))
		for i, a := range accounts {
			accts[i] = accountJSON{
				Alias:   a.Alias,
				Email:   a.Email,
				AddedAt: a.AddedAt,
			}
		}

		output := struct {
			ConfigPath   string        `json:"config_path"`
			ClientID     string        `json:"client_id"`
			ClientSecret string        `json:"client_secret"`
			Accounts     []accountJSON `json:"accounts"`
		}{
			ConfigPath:   config.Path(),
			ClientID:     maskedID,
			ClientSecret: maskedSecret,
			Accounts:     accts,
		}

		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Printf("Config file:    %s\n", config.Path())
	fmt.Printf("Client ID:      %s\n", maskedID)
	fmt.Printf("Client Secret:  %s\n", maskedSecret)
	fmt.Println()

	if len(accounts) == 0 {
		fmt.Println("Accounts: (none)")
	} else {
		fmt.Printf("Accounts (%d):\n", len(accounts))
		for _, a := range accounts {
			fmt.Printf("  %-15s %s  (added %s)\n", a.Alias, a.Email, a.AddedAt)
		}
	}

	return nil
}

// maskString masks a string for display, showing first 4 and last 4 characters.
func maskString(s string) string {
	if s == "" {
		return "(not set)"
	}
	if len(s) > 8 {
		return s[:4] + "..." + s[len(s)-4:]
	}
	return "****"
}
