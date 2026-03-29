// Package cli provides a generic channel notifier that forwards orchestrator
// lifecycle events to external plugin CLIs. Each event is delivered by running:
//
//	<plugin> reply --channel <id> --message <text>
//
// If the plugin binary is not found in PATH, a warning is printed to stderr and
// the notification is silently skipped — plugins are optional, best-effort only.
package cli

import (
	"context"
	"fmt"
	"os"
	"os/exec"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/notify"
)

// defaultEvents are forwarded when the PluginConfig omits the events list.
var defaultEvents = []event.EventType{
	event.MissionStarted,
	event.MissionCompleted,
	event.MissionFailed,
	event.MissionCancelled,
	event.PhaseFailed,
	event.PhaseRetrying,
}

// Notifier delivers orchestrator events to an external plugin CLI.
type Notifier struct {
	plugin     string
	channelIDs []string
	allowed    map[event.EventType]bool
	hook       notify.Hook
}

// New creates a Notifier for the given plugin binary name and config.
func New(plugin string, cfg *notify.PluginConfig) *Notifier {
	events := cfg.Events
	if len(events) == 0 {
		events = defaultEvents
	}
	allowed := make(map[event.EventType]bool, len(events))
	for _, e := range events {
		allowed[e] = true
	}
	return &Notifier{
		plugin:     plugin,
		channelIDs: cfg.ChannelIDs,
		allowed:    allowed,
	}
}

// SetHook attaches optional health-tracking callbacks.
func (n *Notifier) SetHook(h notify.Hook) {
	n.hook = h
}

// Consume reads events from ch and forwards matching ones to the plugin CLI.
// Blocks until ch is closed or ctx is cancelled.
func (n *Notifier) Consume(ctx context.Context, ch <-chan event.Event) {
	for {
		select {
		case <-ctx.Done():
			return
		case ev, ok := <-ch:
			if !ok {
				return
			}
			if n.hook.OnEvent != nil {
				n.hook.OnEvent()
			}
			if !n.allowed[ev.Type] {
				continue
			}
			msg := formatEvent(ev)
			for _, id := range n.channelIDs {
				if err := n.send(ctx, id, msg); err != nil {
					if n.hook.OnError != nil {
						n.hook.OnError(err)
					}
				}
			}
		}
	}
}

// Close is a no-op; the CLI notifier holds no persistent resources.
func (n *Notifier) Close() error { return nil }

// send runs `<plugin> reply --channel <id> --message <msg>`.
// If the binary is not found in PATH, a warning is printed and nil is returned.
func (n *Notifier) send(ctx context.Context, channelID, msg string) error {
	path, err := exec.LookPath(n.plugin)
	if err != nil {
		fmt.Fprintf(os.Stderr, "daemon: notify: %s not found in PATH, skipping\n", n.plugin)
		return nil
	}
	cmd := exec.CommandContext(ctx, path, "reply", "--channel", channelID, "--message", msg)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("%s reply --channel %s: %w: %s", n.plugin, channelID, err, out)
	}
	return nil
}

// formatEvent produces a plain-text summary of ev for the plugin CLI message.
func formatEvent(ev event.Event) string {
	task, _ := ev.Data["task"].(string)
	switch ev.Type {
	case event.MissionStarted:
		if task != "" {
			return fmt.Sprintf("[mission.started] %s — %s", ev.MissionID, task)
		}
		return fmt.Sprintf("[mission.started] %s", ev.MissionID)
	case event.MissionCompleted:
		if task != "" {
			return fmt.Sprintf("[mission.completed] %s — %s", ev.MissionID, task)
		}
		return fmt.Sprintf("[mission.completed] %s", ev.MissionID)
	case event.MissionFailed:
		errMsg, _ := ev.Data["error"].(string)
		if errMsg != "" {
			return fmt.Sprintf("[mission.failed] %s: %s", ev.MissionID, errMsg)
		}
		return fmt.Sprintf("[mission.failed] %s", ev.MissionID)
	case event.MissionCancelled:
		return fmt.Sprintf("[mission.cancelled] %s", ev.MissionID)
	case event.PhaseFailed:
		errMsg, _ := ev.Data["error"].(string)
		if errMsg != "" {
			return fmt.Sprintf("[phase.failed] %s/%s: %s", ev.MissionID, ev.PhaseID, errMsg)
		}
		return fmt.Sprintf("[phase.failed] %s/%s", ev.MissionID, ev.PhaseID)
	case event.PhaseRetrying:
		attempt, _ := ev.Data["attempt"].(float64)
		return fmt.Sprintf("[phase.retrying] %s/%s (attempt %d)", ev.MissionID, ev.PhaseID, int(attempt))
	default:
		return fmt.Sprintf("[%s] %s", ev.Type, ev.MissionID)
	}
}
