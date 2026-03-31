package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"

	"github.com/wailsapp/wails/v2/pkg/runtime"
)

const daemonChatBase = "http://127.0.0.1:7331/api/chat"

var (
	chatCancelMu sync.Mutex
	chatCancel   context.CancelFunc
	chatGeneration uint64
)

// StartChat POSTs a message to the daemon chat endpoint and returns the
// conversation_id. Pass an empty conversationID to start a new conversation.
func (a *App) StartChat(message, conversationID string) (string, error) {
	payload := map[string]any{"message": message}
	if conversationID != "" {
		payload["conversation_id"] = conversationID
	}
	data, _ := json.Marshal(payload)

	resp, err := http.Post(daemonChatBase, "application/json", bytes.NewReader(data))
	if err != nil {
		return "", fmt.Errorf("chat start: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusAccepted && resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("chat start: HTTP %d", resp.StatusCode)
	}

	var result struct {
		ConversationID string `json:"conversation_id"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return "", fmt.Errorf("chat decode: %w", err)
	}
	return result.ConversationID, nil
}

// GetConversation fetches a conversation's message history from the daemon and
// returns it as a JSON string for the frontend to unmarshal.
func (a *App) GetConversation(id string) (string, error) {
	resp, err := http.Get(fmt.Sprintf("%s/%s", daemonChatBase, id))
	if err != nil {
		return "", fmt.Errorf("get conversation: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("get conversation: HTTP %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", err
	}
	return string(body), nil
}

// StreamChat opens the daemon's SSE stream for conversationID and forwards
// events to the frontend via Wails events:
//
//   - "chat:token"  — string (the token text chunk)
//   - "chat:done"   — nil
//   - "chat:error"  — string (the error message)
//
// Any in-progress stream is cancelled before starting the new one.
// Returns immediately; streaming runs in a background goroutine.
func (a *App) StreamChat(conversationID string) {
	chatCancelMu.Lock()
	if chatCancel != nil {
		chatCancel()
	}
	ctx, cancel := context.WithCancel(a.ctx)
	chatCancel = cancel
	chatGeneration++
	myGen := chatGeneration
	chatCancelMu.Unlock()

	go func() {
		defer func() {
			chatCancelMu.Lock()
			if chatGeneration == myGen {
				chatCancel = nil
			}
			chatCancelMu.Unlock()
		}()

		url := fmt.Sprintf("%s/%s/stream", daemonChatBase, conversationID)
		req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
		if err != nil {
			runtime.EventsEmit(a.ctx, "chat:error", err.Error())
			return
		}
		req.Header.Set("Accept", "text/event-stream")
		req.Header.Set("Cache-Control", "no-cache")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			if ctx.Err() == nil {
				runtime.EventsEmit(a.ctx, "chat:error", "stream disconnected")
			}
			return
		}
		defer resp.Body.Close()

		scanner := bufio.NewScanner(resp.Body)
		scanner.Buffer(make([]byte, 1<<20), 1<<20)

		var (
			eventType string
			dataLines []string
		)

		for scanner.Scan() {
			if ctx.Err() != nil {
				return
			}
			line := scanner.Text()
			switch {
			case strings.HasPrefix(line, "event: "):
				eventType = strings.TrimPrefix(line, "event: ")
			case strings.HasPrefix(line, "data: "):
				dataLines = append(dataLines, strings.TrimPrefix(line, "data: "))
			case line == "" && len(dataLines) > 0:
				payload := strings.Join(dataLines, "")
				switch eventType {
				case "token":
					var t struct {
						Text string `json:"text"`
					}
					json.Unmarshal([]byte(payload), &t) //nolint:errcheck
					runtime.EventsEmit(a.ctx, "chat:token", t.Text)
				case "done":
					runtime.EventsEmit(a.ctx, "chat:done", nil)
					return
				case "error":
					var e struct {
						Message string `json:"message"`
					}
					json.Unmarshal([]byte(payload), &e) //nolint:errcheck
					runtime.EventsEmit(a.ctx, "chat:error", e.Message)
					return
				}
				eventType = ""
				dataLines = dataLines[:0]
			}
		}

		if err := scanner.Err(); err != nil && ctx.Err() == nil {
			runtime.EventsEmit(a.ctx, "chat:error", "stream disconnected")
		} else if ctx.Err() == nil {
			// Clean EOF without a "done" frame — treat as done.
			runtime.EventsEmit(a.ctx, "chat:done", nil)
		}
	}()
}
