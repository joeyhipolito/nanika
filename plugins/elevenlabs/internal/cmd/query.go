package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/config"
)

type elevenLabsStatusOutput struct {
	Connected          bool   `json:"connected"`
	VoiceCount         int    `json:"voice_count"`
	CharacterCount     int    `json:"character_count"`
	CharacterLimit     int    `json:"character_limit"`
	SubscriptionStatus string `json:"subscription_status"`
}

type elevenLabsQueryAction struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type elevenLabsQueryActionsOutput struct {
	Actions []elevenLabsQueryAction `json:"actions"`
}

type elevenLabsQueryItemsOutput struct {
	Items []elevenLabsVoiceItem `json:"items"`
	Count int                   `json:"count"`
}

type elevenLabsVoiceItem struct {
	VoiceID   string `json:"voice_id"`
	Name      string `json:"name"`
	Category  string `json:"category"`
	IsDefault bool   `json:"is_default"`
}

func elevenLabsQueryItems(jsonOutput bool) error {
	if !jsonOutput {
		return VoicesCmd(false)
	}
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
	items := make([]elevenLabsVoiceItem, len(voices))
	for i, v := range voices {
		items[i] = elevenLabsVoiceItem{
			VoiceID:   v.VoiceID,
			Name:      v.Name,
			Category:  v.Category,
			IsDefault: v.VoiceID == cfg.DefaultVoiceID,
		}
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(elevenLabsQueryItemsOutput{Items: items, Count: len(items)})
}

func QueryCmd(subcommand string, jsonOutput bool) error {
	switch subcommand {
	case "status":
		return elevenLabsQueryStatus(jsonOutput)
	case "items":
		return elevenLabsQueryItems(jsonOutput)
	case "actions":
		return elevenLabsQueryActions(jsonOutput)
	default:
		return fmt.Errorf("unknown query subcommand %q — use status, items, or actions", subcommand)
	}
}

func elevenLabsQueryStatus(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.APIKey == "" {
		return fmt.Errorf("no API key configured. Run 'elevenlabs configure' first")
	}

	client := api.NewClient(cfg.APIKey)
	ctx := context.Background()

	// Fetch user info and voices concurrently — independent endpoints.
	type userResult struct {
		user api.UserResponse
		err  error
	}
	type voicesResult struct {
		voices []api.Voice
		err    error
	}
	userCh := make(chan userResult, 1)
	voicesCh := make(chan voicesResult, 1)

	go func() {
		u, err := client.GetUser(ctx)
		userCh <- userResult{u, err}
	}()
	go func() {
		v, err := client.GetVoices(ctx)
		voicesCh <- voicesResult{v, err}
	}()

	ur := <-userCh
	vr := <-voicesCh
	if ur.err != nil {
		return fmt.Errorf("checking API connectivity: %w", ur.err)
	}
	if vr.err != nil {
		return fmt.Errorf("fetching voices: %w", vr.err)
	}

	out := elevenLabsStatusOutput{
		Connected:          true,
		VoiceCount:         len(vr.voices),
		CharacterCount:     ur.user.Subscription.CharacterCount,
		CharacterLimit:     ur.user.Subscription.CharacterLimit,
		SubscriptionStatus: ur.user.Subscription.Status,
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	used := 0
	if out.CharacterLimit > 0 {
		used = out.CharacterCount * 100 / out.CharacterLimit
	}
	connected := "no"
	if out.Connected {
		connected = "yes"
	}
	fmt.Println("ElevenLabs Status")
	fmt.Println(strings.Repeat("=", 30))
	fmt.Printf("  Connected:       %s\n", connected)
	fmt.Printf("  Subscription:    %s\n", out.SubscriptionStatus)
	fmt.Printf("  Voices:          %d available\n", out.VoiceCount)
	fmt.Printf("  Characters:      %d / %d used (%d%%)\n", out.CharacterCount, out.CharacterLimit, used)
	return nil
}

func elevenLabsQueryActions(jsonOutput bool) error {
	actions := []elevenLabsQueryAction{
		{Name: "voices", Command: "elevenlabs voices", Description: "List available voices"},
		{Name: "format", Command: "elevenlabs format <script.md>", Description: "Format narration script for TTS"},
		{Name: "generate", Command: "elevenlabs generate <text.txt>", Description: "Generate voiceover audio"},
		{Name: "timing", Command: "elevenlabs timing <map.json>", Description: "Produce clip alignment guide"},
		{Name: "assemble", Command: "elevenlabs assemble <audio>", Description: "Splice silence into voiceover"},
		{Name: "align", Command: "elevenlabs align <audio> <transcript.txt>", Description: "Run forced alignment"},
		{Name: "transcribe", Command: "elevenlabs transcribe --input <file>", Description: "Transcribe audio to text"},
		{Name: "doctor", Command: "elevenlabs doctor", Description: "Verify API key and connectivity"},
	}
	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(elevenLabsQueryActionsOutput{Actions: actions})
	}
	fmt.Println("Available actions:")
	for _, a := range actions {
		fmt.Printf("  %-50s  %s\n", a.Command, a.Description)
	}
	return nil
}
