// Package cmd implements the elevenlabs CLI subcommands.
package cmd

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/config"
)

// ConfigureCmd prompts for API key, default voice ID, and model, then saves config.
func ConfigureCmd() error {
	reader := bufio.NewReader(os.Stdin)

	if config.Exists() {
		fmt.Print("Configuration already exists. Overwrite? [y/N]: ")
		answer, _ := reader.ReadString('\n')
		answer = strings.TrimSpace(strings.ToLower(answer))
		if answer != "y" && answer != "yes" {
			fmt.Println("Configuration unchanged.")
			return nil
		}
	}

	fmt.Println("ElevenLabs CLI Configuration")
	fmt.Println("============================")
	fmt.Println()
	fmt.Println("Get your API key at: https://elevenlabs.io/app/settings/api-keys")
	fmt.Println()

	// Required: API key
	fmt.Print("API key: ")
	apiKey, _ := reader.ReadString('\n')
	apiKey = strings.TrimSpace(apiKey)
	if apiKey == "" {
		return fmt.Errorf("API key is required")
	}

	// Verify the key before saving
	fmt.Println("Verifying API key...")
	client := api.NewClient(apiKey)
	user, err := client.GetUser(context.Background())
	if err != nil {
		return fmt.Errorf("API key verification failed: %w", err)
	}
	used := user.Subscription.CharacterCount
	limit := user.Subscription.CharacterLimit
	fmt.Printf("API key valid. Quota: %d / %d characters used (%s plan)\n",
		used, limit, user.Subscription.Status)
	fmt.Println()

	// Optional: default voice ID
	fmt.Print("Default voice ID (leave blank to pick at generate time): ")
	voiceID, _ := reader.ReadString('\n')
	voiceID = strings.TrimSpace(voiceID)

	// Optional: model (default shown)
	fmt.Printf("Model [%s]: ", config.DefaultModel)
	model, _ := reader.ReadString('\n')
	model = strings.TrimSpace(model)
	if model == "" {
		model = config.DefaultModel
	}

	cfg := config.Config{
		APIKey:         apiKey,
		DefaultVoiceID: voiceID,
		Model:          model,
	}
	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}

	fmt.Printf("\nConfiguration saved to %s\n", config.Path())
	fmt.Println()
	fmt.Println("Next steps:")
	fmt.Println("  elevenlabs doctor         — verify everything is working")
	fmt.Println("  elevenlabs voices         — list available voices")
	fmt.Println("  elevenlabs generate       — generate voiceover audio")
	return nil
}

// ConfigureShowCmd displays the current config with the API key masked.
func ConfigureShowCmd(jsonOutput bool) error {
	if !config.Exists() {
		return fmt.Errorf("no configuration found. Run 'elevenlabs configure' first")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	masked := maskKey(cfg.APIKey)
	p := config.Path()

	if jsonOutput {
		type showOutput struct {
			APIKey         string `json:"api_key"`
			DefaultVoiceID string `json:"default_voice_id"`
			Model          string `json:"model"`
			Path           string `json:"path"`
		}
		out := showOutput{
			APIKey:         masked,
			DefaultVoiceID: cfg.DefaultVoiceID,
			Model:          cfg.Model,
			Path:           p,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Printf("Config path:      %s\n", p)
	fmt.Printf("api_key:          %s\n", masked)
	fmt.Printf("default_voice_id: %s\n", cfg.DefaultVoiceID)
	fmt.Printf("model:            %s\n", cfg.Model)
	return nil
}

// maskKey returns the API key with all but the first 4 and last 4 characters replaced by *.
func maskKey(key string) string {
	if len(key) <= 8 {
		return strings.Repeat("*", len(key))
	}
	return key[:4] + strings.Repeat("*", len(key)-8) + key[len(key)-4:]
}
