package cmd

import (
	"bufio"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
)

// ConfigureCmd runs an interactive configuration setup.
// Prompts for Gemini API key and vault path, writes ~/.obsidian/config.
func ConfigureCmd() error {
	reader := bufio.NewReader(os.Stdin)

	fmt.Println("Obsidian CLI Configuration")
	fmt.Println("==========================")
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
	fmt.Println("Get your Gemini API key from:")
	fmt.Println("https://aistudio.google.com/api-keys")
	fmt.Println()
	if existing.GeminiAPIKey != "" {
		fmt.Printf("Gemini API Key [%s]: ", maskKey(existing.GeminiAPIKey))
	} else {
		fmt.Print("Gemini API Key: ")
	}
	apiKey, _ := reader.ReadString('\n')
	apiKey = strings.TrimSpace(apiKey)
	if apiKey == "" {
		apiKey = existing.GeminiAPIKey
	}
	if apiKey == "" {
		return fmt.Errorf("Gemini API key is required")
	}

	// Prompt for vault path
	fmt.Println()
	fmt.Println("Path to your Obsidian vault:")
	if existing.VaultPath != "" {
		fmt.Printf("Vault path [%s]: ", existing.VaultPath)
	} else {
		fmt.Print("Vault path: ")
	}
	vaultPath, _ := reader.ReadString('\n')
	vaultPath = strings.TrimSpace(vaultPath)
	if vaultPath == "" {
		vaultPath = existing.VaultPath
	}
	if vaultPath == "" {
		return fmt.Errorf("vault path is required")
	}

	// Expand ~ in vault path
	vaultPath = expandHome(vaultPath)

	// Validate vault path exists
	info, err := os.Stat(vaultPath)
	if err != nil || !info.IsDir() {
		return fmt.Errorf("vault path does not exist or is not a directory: %s", vaultPath)
	}

	// Prompt for optional second-brain vault path (skippable).
	fmt.Println()
	fmt.Println("Optional: path to your hand-curated second-brain vault")
	fmt.Println("(used by `obsidian --vault second-brain ...`; press Enter to skip)")
	if existing.SecondBrainPath != "" {
		fmt.Printf("Second-brain path [%s]: ", existing.SecondBrainPath)
	} else {
		fmt.Print("Second-brain path: ")
	}
	secondBrainPath, _ := reader.ReadString('\n')
	secondBrainPath = strings.TrimSpace(secondBrainPath)
	if secondBrainPath == "" {
		secondBrainPath = existing.SecondBrainPath
	}
	if secondBrainPath != "" {
		secondBrainPath = expandHome(secondBrainPath)
		sbInfo, sbErr := os.Stat(secondBrainPath)
		if sbErr != nil || !sbInfo.IsDir() {
			return fmt.Errorf("second-brain path does not exist or is not a directory: %s", secondBrainPath)
		}
	}

	// Save configuration. Preserve optional fields that this prompt does not
	// set, so reconfiguring doesn't wipe out website/recall settings.
	cfg := &config.Config{
		GeminiAPIKey:    apiKey,
		VaultPath:       vaultPath,
		SecondBrainPath: secondBrainPath,
		WebsitePath:     existing.WebsitePath,
		WebsiteURL:      existing.WebsiteURL,
		RecallSocket:    existing.RecallSocket,
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("failed to save configuration: %w", err)
	}

	fmt.Println()
	fmt.Printf("Configuration saved to %s\n", config.Path())
	fmt.Println()
	fmt.Println("Test your setup:")
	fmt.Println("  obsidian list")
	fmt.Println("  obsidian search \"test\"")
	fmt.Println()
	fmt.Println("Troubleshoot:")
	fmt.Println("  obsidian doctor")

	return nil
}

// ConfigureShowCmd prints the current configuration (with API key masked).
func ConfigureShowCmd(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	if !config.Exists() {
		fmt.Println("No configuration file found.")
		fmt.Println("Run 'obsidian configure' to set up.")
		return nil
	}

	maskedKey := maskKey(cfg.GeminiAPIKey)

	if jsonOutput {
		return output.JSON(map[string]string{
			"config_path":       config.Path(),
			"gemini_apikey":     maskedKey,
			"vault_path":        cfg.VaultPath,
			"second_brain_path": cfg.SecondBrainPath,
		})
	}

	fmt.Printf("Config file: %s\n", config.Path())
	fmt.Printf("Gemini API key: %s\n", maskedKey)
	fmt.Printf("Vault path: %s\n", cfg.VaultPath)
	if cfg.SecondBrainPath != "" {
		fmt.Printf("Second-brain path: %s\n", cfg.SecondBrainPath)
	}
	return nil
}

// expandHome replaces a leading "~/" with the user's home directory. Returns
// the input unchanged if expansion fails or the prefix is absent.
func expandHome(p string) string {
	if !strings.HasPrefix(p, "~/") {
		return p
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return p
	}
	return home + p[1:]
}

// maskKey returns a masked version of an API key for display.
func maskKey(key string) string {
	if key == "" {
		return ""
	}
	if len(key) > 8 {
		return key[:4] + "..." + key[len(key)-4:]
	}
	return "****"
}
