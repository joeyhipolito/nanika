package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"

	discord "github.com/joeyhipolito/nanika-discord/internal"
)

const version = "0.1.0"

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}

func emit(v any) {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.Encode(v) //nolint:errcheck
}

func getFlag(args []string, name string) string {
	for i, a := range args {
		if a == name && i+1 < len(args) {
			return args[i+1]
		}
	}
	return ""
}

func run(args []string) error {
	if len(args) == 0 {
		fmt.Print("discord — send messages and voice messages to Discord\n\n" +
			"Commands:\n" +
			"  send-voice-message --channel <id> --audio <path>  Send a native voice message\n" +
			"  reply --channel <id> --message <text>             Send a text message\n" +
			"  query status|items|actions                         Dashboard protocol\n" +
			"  doctor                                             Health check\n")
		return nil
	}

	jsonOut := false
	filtered := make([]string, 0, len(args))
	for _, a := range args {
		if a == "--json" {
			jsonOut = true
		} else {
			filtered = append(filtered, a)
		}
	}
	args = filtered

	switch args[0] {
	case "send-voice-message":
		return cmdSendVoiceMessage(args[1:], jsonOut)
	case "reply":
		return cmdReply(args[1:], jsonOut)
	case "query":
		if len(args) < 2 {
			return fmt.Errorf("usage: discord query <status|items|actions>")
		}
		switch args[1] {
		case "status":
			return cmdQueryStatus(jsonOut)
		case "items":
			return cmdQueryItems(jsonOut)
		case "actions":
			return cmdQueryActions(jsonOut)
		}
		return fmt.Errorf("unknown query subcommand: %s", args[1])
	case "doctor":
		return cmdDoctor(jsonOut)
	}
	return fmt.Errorf("unknown command: %s (try: send-voice-message, query, doctor)", args[0])
}

func cmdSendVoiceMessage(args []string, jsonOut bool) error {
	channelID := getFlag(args, "--channel")
	audioPath := getFlag(args, "--audio")
	if channelID == "" {
		return fmt.Errorf("--channel <id> is required")
	}
	if audioPath == "" {
		return fmt.Errorf("--audio <path> is required")
	}
	cfg, err := discord.LoadConfig()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	client := discord.NewClient(cfg.BotToken)
	if err := client.SendVoiceMessage(context.Background(), channelID, audioPath); err != nil {
		return err
	}
	if jsonOut {
		emit(map[string]any{"ok": true, "channel_id": channelID})
	} else {
		fmt.Printf("Voice message sent to channel %s\n", channelID)
	}
	return nil
}

func cmdReply(args []string, jsonOut bool) error {
	channelID := getFlag(args, "--channel")
	message := getFlag(args, "--message")
	if channelID == "" {
		return fmt.Errorf("--channel <id> is required")
	}
	if message == "" {
		return fmt.Errorf("--message <text> is required")
	}
	cfg, err := discord.LoadConfig()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	client := discord.NewClient(cfg.BotToken)
	if err := client.SendMessage(context.Background(), channelID, message); err != nil {
		return err
	}
	if jsonOut {
		emit(map[string]any{"ok": true, "channel_id": channelID})
	} else {
		fmt.Printf("Message sent to channel %s\n", channelID)
	}
	return nil
}

func cmdQueryStatus(jsonOut bool) error {
	cfg, err := discord.LoadConfig()
	if err != nil {
		if jsonOut {
			emit(map[string]any{"ok": false, "error": err.Error(), "version": version})
			return nil
		}
		return err
	}
	tokenHint := cfg.BotToken
	if len(tokenHint) > 8 {
		tokenHint = tokenHint[:8] + "..."
	}
	if jsonOut {
		emit(map[string]any{
			"ok":           true,
			"version":      version,
			"channels":     len(cfg.ChannelIDs),
			"token_prefix": tokenHint,
		})
	} else {
		fmt.Printf("version=%s  channels=%d  status=ok\n", version, len(cfg.ChannelIDs))
	}
	return nil
}

func cmdQueryItems(jsonOut bool) error {
	cfg, err := discord.LoadConfig()
	if err != nil {
		if jsonOut {
			emit([]any{})
			return nil
		}
		return err
	}
	items := make([]map[string]any, len(cfg.ChannelIDs))
	for i, id := range cfg.ChannelIDs {
		items[i] = map[string]any{"id": id, "type": "channel"}
	}
	if jsonOut {
		emit(items)
	} else {
		for _, id := range cfg.ChannelIDs {
			fmt.Printf("channel: %s\n", id)
		}
	}
	return nil
}

func cmdQueryActions(jsonOut bool) error {
	actions := []map[string]any{
		{
			"name":        "send-voice-message",
			"command":     "discord send-voice-message --channel <id> --audio <path>",
			"description": "Send an audio file as a native Discord voice message",
		},
		{
			"name":        "reply",
			"command":     "discord reply --channel <id> --message <text>",
			"description": "Send a plain text message to a Discord channel",
		},
	}
	if jsonOut {
		emit(map[string]any{"actions": actions})
	} else {
		fmt.Println("send-voice-message --channel <id> --audio <path>  — send a native voice message")
		fmt.Println("reply --channel <id> --message <text>               — send a text message")
	}
	return nil
}

func cmdDoctor(jsonOut bool) error {
	checks := []map[string]any{}

	ffmpegStatus, ffmpegMsg := "ok", "ffmpeg found in PATH"
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		ffmpegStatus, ffmpegMsg = "error", "ffmpeg not found: brew install ffmpeg"
	}
	checks = append(checks, map[string]any{"name": "ffmpeg", "status": ffmpegStatus, "message": ffmpegMsg})

	cfgStatus, cfgMsg := "ok", "config loaded"
	if _, err := discord.LoadConfig(); err != nil {
		cfgStatus, cfgMsg = "error", err.Error()
	}
	checks = append(checks, map[string]any{"name": "config", "status": cfgStatus, "message": cfgMsg})

	ok := ffmpegStatus == "ok" && cfgStatus == "ok"
	if jsonOut {
		emit(map[string]any{"ok": ok, "checks": checks})
	} else {
		for _, c := range checks {
			fmt.Printf("[%s] %s: %s\n", c["status"], c["name"], c["message"])
		}
	}
	return nil
}
