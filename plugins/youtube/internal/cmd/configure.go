package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/joeyhipolito/nanika-youtube/internal/api"
)

// ConfigureCmd handles configure and its sub-commands.
//
//	youtube configure         — interactive config creation
//	youtube configure show    — print current config (masked)
func ConfigureCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 && args[0] == "show" {
		return configureShowCmd(jsonOutput)
	}
	return configureInteractiveCmd()
}

func configureInteractiveCmd() error {
	reader := bufio.NewReader(os.Stdin)

	// Load existing config to preserve unmodified fields.
	existing := api.Config{}
	if api.ConfigExists() {
		existing, _ = api.LoadConfig()
	}

	fmt.Println("YouTube CLI — Configuration")
	fmt.Println("============================")
	fmt.Println()
	fmt.Println("You'll need a Google Cloud project with the YouTube Data API v3 enabled.")
	fmt.Println("Create credentials at: https://console.cloud.google.com/apis/credentials")
	fmt.Println()

	prompt := func(label, current string) (string, error) {
		if current != "" {
			fmt.Printf("%s [current: %s]: ", label, current)
		} else {
			fmt.Printf("%s: ", label)
		}
		val, err := reader.ReadString('\n')
		if err != nil {
			return "", fmt.Errorf("reading input: %w", err)
		}
		val = strings.TrimSpace(val)
		if val == "" {
			return current, nil // keep existing value
		}
		return val, nil
	}

	apiKey, err := prompt("API Key (for scan/search, no OAuth required)", existing.APIKey)
	if err != nil {
		return err
	}

	clientID, err := prompt("OAuth Client ID (for posting comments/likes)", existing.ClientID)
	if err != nil {
		return err
	}

	clientSecret, err := prompt("OAuth Client Secret", existing.ClientSecret)
	if err != nil {
		return err
	}

	fmt.Printf("Quota budget (units/day) [current: %d, default 10000]: ", existing.Budget)
	budgetStr, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("reading input: %w", err)
	}
	budgetStr = strings.TrimSpace(budgetStr)
	budget := existing.Budget
	if budgetStr != "" {
		b, err := strconv.Atoi(budgetStr)
		if err != nil {
			return fmt.Errorf("invalid budget: %s", budgetStr)
		}
		budget = b
	}
	if budget <= 0 {
		budget = 10000
	}

	cfg := api.Config{
		APIKey:       apiKey,
		ClientID:     clientID,
		ClientSecret: clientSecret,
		Channels:     existing.Channels,
		Budget:       budget,
	}

	if err := api.SaveConfig(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}

	path, _ := api.ConfigFilePath()
	fmt.Printf("\nConfiguration saved to %s\n", path)
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  youtube auth       — set up OAuth for posting comments and likes")
	fmt.Println("  youtube doctor     — verify everything is working")
	return nil
}

type configShowOutput struct {
	APIKey       string   `json:"api_key"`
	ClientID     string   `json:"client_id"`
	ClientSecret string   `json:"client_secret"`
	Channels     []string `json:"channels"`
	Budget       int      `json:"budget"`
	Path         string   `json:"path"`
}

func configureShowCmd(jsonOutput bool) error {
	if !api.ConfigExists() {
		return fmt.Errorf("not configured. Run 'youtube configure' first")
	}

	cfg, err := api.LoadConfig()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	path, _ := api.ConfigFilePath()

	if jsonOutput {
		out := configShowOutput{
			APIKey:       maskValue(cfg.APIKey),
			ClientID:     cfg.ClientID,
			ClientSecret: maskValue(cfg.ClientSecret),
			Channels:     cfg.Channels,
			Budget:       cfg.Budget,
			Path:         path,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Println("YouTube Configuration")
	fmt.Println("=====================")
	fmt.Println()
	fmt.Printf("  API Key:       %s\n", maskValue(cfg.APIKey))
	fmt.Printf("  Client ID:     %s\n", cfg.ClientID)
	fmt.Printf("  Client Secret: %s\n", maskValue(cfg.ClientSecret))
	fmt.Printf("  Budget:        %d quota units/day\n", cfg.Budget)
	if len(cfg.Channels) > 0 {
		fmt.Printf("  Channels:      %s\n", strings.Join(cfg.Channels, ", "))
	} else {
		fmt.Println("  Channels:      (none configured)")
	}
	fmt.Println()
	fmt.Printf("Config path: %s\n", path)
	return nil
}

func maskValue(v string) string {
	if v == "" {
		return "(not set)"
	}
	if len(v) < 12 {
		return "****"
	}
	return v[:8] + "..." + v[len(v)-4:]
}
