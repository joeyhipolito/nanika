package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-scout/internal/config"
)

// ConfigureCmd runs an interactive configuration setup.
func ConfigureCmd() error {
	reader := bufio.NewReader(os.Stdin)

	fmt.Println("Scout CLI Configuration")
	fmt.Println("=======================")
	fmt.Println()

	// Check for existing config
	if config.Exists() {
		fmt.Printf("Existing configuration found at %s\n", config.Path())
		fmt.Print("Overwrite? [y/N] ")
		reply, _ := reader.ReadString('\n')
		reply = strings.TrimSpace(reply)
		if !strings.EqualFold(reply, "y") {
			fmt.Println("Configuration cancelled.")
			return nil
		}
		fmt.Println()
	}

	// Load existing config for defaults
	existing, _ := config.Load()

	// Prompt for Gemini API key
	defaultGeminiKey := existing.GeminiAPIKey
	if defaultGeminiKey != "" {
		fmt.Printf("Gemini API key [%s]: ", maskKey(defaultGeminiKey))
	} else {
		fmt.Print("Gemini API key (optional, from https://aistudio.google.com/apikey): ")
	}
	geminiAPIKey, _ := reader.ReadString('\n')
	geminiAPIKey = strings.TrimSpace(geminiAPIKey)
	if geminiAPIKey == "" {
		geminiAPIKey = defaultGeminiKey
	}

	// Save configuration
	cfg := &config.Config{
		GeminiAPIKey: geminiAPIKey,
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("failed to save configuration: %w", err)
	}

	// Also ensure directories exist
	if err := config.EnsureDirs(); err != nil {
		return fmt.Errorf("failed to create directories: %w", err)
	}

	fmt.Println()
	fmt.Printf("Configuration saved to %s\n", config.Path())
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  scout topics preset ai-all")
	fmt.Println("  scout topics add \"my-topic\" --sources \"rss,web\" --feeds \"https://example.com/feed.xml\"")
	fmt.Println("  scout gather")
	fmt.Println()
	fmt.Println("Troubleshoot:")
	fmt.Println("  scout doctor")

	return nil
}

// maskKey returns the last 4 chars of a key with the rest masked, e.g. "****abcd".
func maskKey(key string) string {
	if len(key) <= 4 {
		return strings.Repeat("*", len(key))
	}
	return strings.Repeat("*", len(key)-4) + key[len(key)-4:]
}

// ConfigureShowCmd prints the current configuration.
func ConfigureShowCmd(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	if !config.Exists() {
		fmt.Println("No configuration file found.")
		fmt.Println("Run 'scout configure' to set up.")
		return nil
	}

	geminiStatus := "not set"
	if cfg.GeminiAPIKey != "" {
		geminiStatus = maskKey(cfg.GeminiAPIKey)
	}

	if jsonOutput {
		output := map[string]string{
			"config_path":     config.Path(),
			"gather_interval": cfg.GatherInterval,
			"gemini_apikey":   geminiStatus,
			"topics_dir":      config.TopicsDir(),
			"intel_dir":       config.IntelDir(),
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Printf("Config file:     %s\n", config.Path())
	fmt.Printf("Gather interval: %s\n", cfg.GatherInterval)
	fmt.Printf("Gemini API key:  %s\n", geminiStatus)
	fmt.Printf("Topics dir:      %s\n", config.TopicsDir())
	fmt.Printf("Intel dir:       %s\n", config.IntelDir())
	return nil
}
