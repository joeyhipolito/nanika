package main

import (
	"bufio"
	"context"
	"fmt"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/wailsapp/wails/v2/pkg/runtime"
)

const (
	daemonEventsURL     = "http://127.0.0.1:7331/api/events"
	eventReconnectDelay = 3 * time.Second
)

// startEventBridge connects to the daemon HTTP SSE endpoint and forwards
// events to the Wails frontend via runtime.EventsEmit. Runs until ctx is
// cancelled (app shutdown). Automatically reconnects on failure.
func (a *App) startEventBridge(ctx context.Context) {
	go func() {
		for {
			select {
			case <-ctx.Done():
				return
			default:
			}

			err := a.consumeEventStream(ctx)
			if ctx.Err() != nil {
				return
			}
			if err != nil {
				fmt.Fprintf(os.Stderr, "event bridge: %v; reconnecting in %s\n", err, eventReconnectDelay)
			}

			select {
			case <-ctx.Done():
				return
			case <-time.After(eventReconnectDelay):
			}
		}
	}()
}

// consumeEventStream opens the daemon's SSE endpoint, reads newline-delimited
// SSE frames, and emits each event payload to the frontend.
//
// Emits:
//   - "orchestrator:event"     — raw JSON string of each event
//   - "orchestrator:connected" — bool: true on connect, false on disconnect
func (a *App) consumeEventStream(ctx context.Context) error {
	req, err := http.NewRequestWithContext(ctx, "GET", daemonEventsURL, nil)
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Accept", "text/event-stream")
	req.Header.Set("Cache-Control", "no-cache")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return fmt.Errorf("connecting to daemon: %w", err)
	}
	defer resp.Body.Close()

	runtime.EventsEmit(a.ctx, "orchestrator:connected", true)
	defer runtime.EventsEmit(a.ctx, "orchestrator:connected", false)

	// Use a 1MB scanner buffer — worker output chunks can be large.
	scanner := bufio.NewScanner(resp.Body)
	scanner.Buffer(make([]byte, 1<<20), 1<<20)

	var dataLines []string
	for scanner.Scan() {
		if ctx.Err() != nil {
			return nil
		}
		line := scanner.Text()
		switch {
		case strings.HasPrefix(line, "data: "):
			dataLines = append(dataLines, line[6:])
		case line == "" && len(dataLines) > 0:
			runtime.EventsEmit(a.ctx, "orchestrator:event", strings.Join(dataLines, ""))
			dataLines = dataLines[:0]
		// id:, event:, and keepalive ": ..." lines are intentionally ignored.
		// The orchestrator event type is encoded in the JSON payload's "type" field.
		}
	}
	return scanner.Err()
}
