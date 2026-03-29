package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"

	telegram "github.com/joeyhipolito/nanika-telegram/internal"
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
		fmt.Print("telegram — send messages and voice messages to Telegram\n\n" +
			"Commands:\n" +
			"  send-voice-message --chat <id> --audio <path>  Send a voice message\n" +
			"  reply --chat <id> --message <text>             Send a text message\n" +
			"  query status|items|actions                      Dashboard protocol\n" +
			"  doctor                                          Health check\n")
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
			return fmt.Errorf("usage: telegram query <status|items|actions|action>")
		}
		switch args[1] {
		case "status":
			return cmdQueryStatus(jsonOut)
		case "items":
			return cmdQueryItems(jsonOut)
		case "actions":
			return cmdQueryActions(jsonOut)
		case "action":
			if len(args) < 3 {
				return fmt.Errorf("usage: telegram query action <verb> [payload]")
			}
			payload := ""
			if len(args) > 3 {
				payload = args[3]
			}
			return cmdQueryAction(args[2], payload, jsonOut)
		}
		return fmt.Errorf("unknown query subcommand: %s", args[1])
	case "doctor":
		return cmdDoctor(jsonOut)
	}
	return fmt.Errorf("unknown command: %s (try: send-voice-message, reply, query, doctor)", args[0])
}

func cmdSendVoiceMessage(args []string, jsonOut bool) error {
	chatID := getFlag(args, "--chat")
	audioPath := getFlag(args, "--audio")
	if chatID == "" {
		return fmt.Errorf("--chat <id> is required")
	}
	if audioPath == "" {
		return fmt.Errorf("--audio <path> is required")
	}
	cfg, err := telegram.LoadConfig()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	client := telegram.NewClient(cfg.BotToken)
	if err := client.SendVoice(context.Background(), chatID, audioPath); err != nil {
		return err
	}
	if jsonOut {
		emit(map[string]any{"ok": true, "chat_id": chatID})
	} else {
		fmt.Printf("Voice message sent to chat %s\n", chatID)
	}
	return nil
}

func cmdReply(args []string, jsonOut bool) error {
	chatID := getFlag(args, "--chat")
	if chatID == "" {
		chatID = getFlag(args, "--channel") // accept --channel as alias (daemon notifier contract)
	}
	message := getFlag(args, "--message")
	if chatID == "" {
		return fmt.Errorf("--chat <id> is required")
	}
	if message == "" {
		return fmt.Errorf("--message <text> is required")
	}
	cfg, err := telegram.LoadConfig()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	client := telegram.NewClient(cfg.BotToken)
	if err := client.SendMessage(context.Background(), chatID, message); err != nil {
		return err
	}
	if jsonOut {
		emit(map[string]any{"ok": true, "chat_id": chatID})
	} else {
		fmt.Printf("Message sent to chat %s\n", chatID)
	}
	return nil
}

func cmdQueryStatus(jsonOut bool) error {
	cfg, err := telegram.LoadConfig()
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
			"chats":        len(cfg.ChatIDs),
			"token_prefix": tokenHint,
		})
	} else {
		fmt.Printf("version=%s  chats=%d  status=ok\n", version, len(cfg.ChatIDs))
	}
	return nil
}

func cmdQueryItems(jsonOut bool) error {
	cfg, err := telegram.LoadConfig()
	if err != nil {
		if jsonOut {
			emit([]any{})
			return nil
		}
		return err
	}
	items := make([]map[string]any, len(cfg.ChatIDs))
	for i, id := range cfg.ChatIDs {
		items[i] = map[string]any{"id": id, "type": "chat"}
	}
	if jsonOut {
		emit(items)
	} else {
		for _, id := range cfg.ChatIDs {
			fmt.Printf("chat: %s\n", id)
		}
	}
	return nil
}

func cmdQueryActions(jsonOut bool) error {
	actions := []map[string]any{
		{
			"name":        "send-voice-message",
			"command":     "telegram send-voice-message --chat <id> --audio <path>",
			"description": "Send an audio file as a Telegram voice message",
		},
		{
			"name":        "reply",
			"command":     "telegram reply --chat <id> --message <text>",
			"description": "Send a plain text message to a Telegram chat",
		},
	}
	if jsonOut {
		emit(map[string]any{"actions": actions})
	} else {
		fmt.Println("send-voice-message --chat <id> --audio <path>  — send a voice message")
		fmt.Println("reply --chat <id> --message <text>               — send a text message")
	}
	return nil
}

func cmdQueryAction(verb, payload string, jsonOut bool) error {
	cfg, err := telegram.LoadConfig()
	if err != nil {
		if jsonOut {
			emit(map[string]any{"ok": false, "error": err.Error()})
			return nil
		}
		return err
	}
	client := telegram.NewClient(cfg.BotToken)

	switch verb {
	case "send":
		var p struct {
			Chat string `json:"chat"`
			Text string `json:"text"`
		}
		if err := json.Unmarshal([]byte(payload), &p); err != nil {
			if jsonOut {
				emit(map[string]any{"ok": false, "error": "invalid payload: " + err.Error()})
				return nil
			}
			return fmt.Errorf("parsing payload: %w", err)
		}
		if p.Chat == "" || p.Text == "" {
			if jsonOut {
				emit(map[string]any{"ok": false, "error": "chat and text are required"})
				return nil
			}
			return fmt.Errorf("chat and text are required")
		}
		if err := client.SendMessage(context.Background(), p.Chat, p.Text); err != nil {
			if jsonOut {
				emit(map[string]any{"ok": false, "error": err.Error()})
				return nil
			}
			return err
		}
		if jsonOut {
			emit(map[string]any{"ok": true, "chat_id": p.Chat})
		} else {
			fmt.Printf("Message sent to chat %s\n", p.Chat)
		}

	case "send-voice":
		var p struct {
			Chat  string `json:"chat"`
			Audio string `json:"audio"`
		}
		if err := json.Unmarshal([]byte(payload), &p); err != nil {
			if jsonOut {
				emit(map[string]any{"ok": false, "error": "invalid payload: " + err.Error()})
				return nil
			}
			return fmt.Errorf("parsing payload: %w", err)
		}
		if p.Chat == "" || p.Audio == "" {
			if jsonOut {
				emit(map[string]any{"ok": false, "error": "chat and audio are required"})
				return nil
			}
			return fmt.Errorf("chat and audio are required")
		}
		if err := client.SendVoice(context.Background(), p.Chat, p.Audio); err != nil {
			if jsonOut {
				emit(map[string]any{"ok": false, "error": err.Error()})
				return nil
			}
			return err
		}
		if jsonOut {
			emit(map[string]any{"ok": true, "chat_id": p.Chat})
		} else {
			fmt.Printf("Voice message sent to chat %s\n", p.Chat)
		}

	default:
		if jsonOut {
			emit(map[string]any{"ok": false, "error": "unknown action: " + verb})
		} else {
			return fmt.Errorf("unknown action: %s (supported: send, send-voice)", verb)
		}
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
	var botUsername string
	cfg, err := telegram.LoadConfig()
	if err != nil {
		cfgStatus, cfgMsg = "error", err.Error()
	}
	checks = append(checks, map[string]any{"name": "config", "status": cfgStatus, "message": cfgMsg})

	apiStatus, apiMsg := "ok", "bot reachable"
	if cfg != nil {
		client := telegram.NewClient(cfg.BotToken)
		info, err := client.GetMe(context.Background())
		if err != nil {
			apiStatus, apiMsg = "error", err.Error()
		} else {
			botUsername = "@" + info.Username
			apiMsg = "bot reachable: " + botUsername
		}
	} else {
		apiStatus, apiMsg = "skip", "skipped (config not loaded)"
	}
	checks = append(checks, map[string]any{"name": "api", "status": apiStatus, "message": apiMsg})

	ok := ffmpegStatus == "ok" && cfgStatus == "ok" && apiStatus == "ok"
	if jsonOut {
		result := map[string]any{"ok": ok, "checks": checks}
		if botUsername != "" {
			result["bot"] = botUsername
		}
		emit(result)
	} else {
		for _, c := range checks {
			fmt.Printf("[%s] %s: %s\n", c["status"], c["name"], c["message"])
		}
	}
	return nil
}
