// Package notify defines the channel notification interface for the daemon.
// Implementations push orchestrator lifecycle events to external channels.
// Notifications are best-effort — delivery failures are logged and counted
// but never propagate to the daemon or engine.
package notify

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// Notifier pushes orchestrator events to an external channel.
// Implementations must be safe for concurrent use.
type Notifier interface {
	// Consume reads events from ch and delivers matching ones to the channel.
	// Blocks until ch is closed or ctx is cancelled. The daemon calls this
	// in a dedicated goroutine after subscribing to the bus.
	Consume(ctx context.Context, ch <-chan event.Event)
	// Close flushes pending messages and releases resources.
	Close() error
}

// Hook is an optional set of callbacks the daemon can attach to a notifier
// for health tracking. Both fields are optional; nil functions are silently skipped.
type Hook struct {
	// OnEvent is called after each event is received from the bus (before delivery).
	OnEvent func()
	// OnError is called when the notifier fails to deliver an event.
	OnError func(error)
}

// PluginConfig holds configuration for a plugin-based channel notifier.
// The plugin binary is located by name in PATH; channel_ids are passed via
// --channel flags. No credentials are stored here — each plugin manages its
// own auth (e.g. via ~/.alluka/channels/<plugin>-auth.json).
type PluginConfig struct {
	ChannelIDs []string          `json:"channel_ids"`
	Events     []event.EventType `json:"events"`
}

// LoadPluginConfig reads ~/.alluka/channels/<plugin>.json and returns the
// parsed PluginConfig. Returns nil, nil when the file does not exist
// (notifications are opt-in).
func LoadPluginConfig(plugin string) (*PluginConfig, error) {
	d, err := config.Dir()
	if err != nil {
		return nil, fmt.Errorf("getting config dir: %w", err)
	}
	path := filepath.Join(d, "channels", plugin+".json")
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return nil, nil // opt-in: no config = no notifications
	}
	if err != nil {
		return nil, fmt.Errorf("reading %s config %s: %w", plugin, path, err)
	}
	var cfg PluginConfig
	if err := json.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parsing %s config %s: %w", plugin, path, err)
	}
	if len(cfg.ChannelIDs) == 0 {
		return nil, fmt.Errorf("%s config %s: channel_ids is required", plugin, path)
	}
	return &cfg, nil
}
