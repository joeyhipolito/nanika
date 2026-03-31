package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/config"
)

// VoicesCmd lists all available voices, marking the configured default.
func VoicesCmd(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.APIKey == "" {
		return fmt.Errorf("no API key configured. Run 'elevenlabs configure' first")
	}

	client := api.NewClient(cfg.APIKey)
	voices, err := client.GetVoices(context.Background())
	if err != nil {
		return fmt.Errorf("fetching voices: %w", err)
	}

	if jsonOutput {
		type voiceOutput struct {
			VoiceID  string `json:"voice_id"`
			Name     string `json:"name"`
			Category string `json:"category"`
			IsDefault bool  `json:"is_default"`
		}
		output := make([]voiceOutput, len(voices))
		for i, v := range voices {
			output[i] = voiceOutput{
				VoiceID:   v.VoiceID,
				Name:      v.Name,
				Category:  v.Category,
				IsDefault: v.VoiceID == cfg.DefaultVoiceID,
			}
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(output)
	}

	fmt.Println("Available Voices")
	fmt.Println("================")
	fmt.Println()
	for _, v := range voices {
		marker := ""
		if v.VoiceID == cfg.DefaultVoiceID {
			marker = " ← default"
		}
		fmt.Printf("%s %q%s\n", v.VoiceID, v.Name, marker)
		if v.Category != "" {
			fmt.Printf("  Category: %s\n", v.Category)
		}
	}

	fmt.Println()
	fmt.Printf("Total: %d voices\n", len(voices))
	if cfg.DefaultVoiceID == "" {
		fmt.Println()
		fmt.Printf("Tip: Set a default voice with: elevenlabs configure\n")
	}
	return nil
}
