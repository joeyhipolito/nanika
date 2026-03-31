package main

import (
	"bufio"
	"context"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"github.com/wailsapp/wails/v2/pkg/runtime"
)

const (
	daemonLiveURL      = "http://127.0.0.1:7331/api/missions/live"
	liveReconnectDelay = 3 * time.Second
)

// startLiveBridge connects to the daemon's global worker-output SSE stream and
// forwards projected liveOutputEvent payloads to the Wails frontend via
// runtime.EventsEmit. Runs until ctx is cancelled (app shutdown).
// Auto-reconnects on failure, matching the pattern in events.go.
//
// Emits:
//   - "worker:output" — JSON string (compact liveOutputEvent: mission_id, phase, persona,
//     tool_name, file_path, chunk, streaming, duration)
func (a *App) startLiveBridge(ctx context.Context) {
	go func() {
		for {
			select {
			case <-ctx.Done():
				return
			default:
			}

			err := a.consumeLiveStream(ctx)
			if ctx.Err() != nil {
				return
			}
			if err != nil {
				fmt.Fprintf(os.Stderr, "live bridge: %v; reconnecting in %s\n", err, liveReconnectDelay)
			}

			select {
			case <-ctx.Done():
				return
			case <-time.After(liveReconnectDelay):
			}
		}
	}()
}

// consumeLiveStream opens the daemon's global live SSE endpoint, reads
// newline-delimited SSE frames, and emits each worker.output payload to
// the frontend as a "worker:output" Wails event.
func (a *App) consumeLiveStream(ctx context.Context) error {
	req, err := http.NewRequestWithContext(ctx, "GET", daemonLiveURL, nil)
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
			runtime.EventsEmit(a.ctx, "worker:output", strings.Join(dataLines, ""))
			dataLines = dataLines[:0]
		// id:, event:, and keepalive ": ..." lines are intentionally ignored.
		// The event type is always worker.output on this endpoint.
		}
	}
	return scanner.Err()
}

// LiveMissionURL returns the full URL for the per-mission worker-output SSE
// stream, for use by the SSE fallback path in the frontend.
// The returned value is safe to embed in EventSource URLs.
func (a *App) LiveMissionURL(missionID string) string {
	return "http://127.0.0.1:7331/api/missions/" + url.PathEscape(missionID) + "/live"
}
