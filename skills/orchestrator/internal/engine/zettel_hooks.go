// zettel_hooks.go — RFC §7 Phase 1 (TRK-525): hook that writes mission Zettels
// by shelling out to the obsidian CLI on MissionCompleted events.
package engine

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// MissionPayload is the JSON body sent to `obsidian zettel write mission`.
type MissionPayload struct {
	ID        string    `json:"id"`
	Slug      string    `json:"slug"`
	Completed time.Time `json:"completed"`
	Personas  []string  `json:"personas"`
	Trackers  []string  `json:"trackers"`
	Artifacts []string  `json:"artifacts"`
	IdeaSlug  string    `json:"idea_slug,omitempty"`
}

// ZettelWriteResult is the JSON response from `obsidian zettel write mission`.
type ZettelWriteResult struct {
	Path        string   `json:"path"`
	Skipped     bool     `json:"skipped"`
	SkipReason  string   `json:"skip_reason,omitempty"`
	Dropped     bool     `json:"dropped,omitempty"`
	DroppedPath string   `json:"dropped_path,omitempty"`
	Warnings    []string `json:"warnings,omitempty"`
	Error       string   `json:"error,omitempty"`
}

// ZettelCLI is the interface the hook uses to write mission Zettels.
// The production implementation shells out to obsidian-cli; tests inject stubs.
type ZettelCLI interface {
	WriteMission(ctx context.Context, payload MissionPayload) (ZettelWriteResult, error)
}

// ExecZettelCLI implements ZettelCLI by running the obsidian binary.
type ExecZettelCLI struct {
	// BinPath is the path to the obsidian binary.
	BinPath string
	// DroppedDir is passed as --dropped-dir to the binary so it can stage
	// failed writes outside the vault when the vault is read-only.
	DroppedDir string
}

// WriteMission marshals payload to JSON, passes it to obsidian via stdin,
// and parses the JSON response from stdout.
func (c *ExecZettelCLI) WriteMission(ctx context.Context, payload MissionPayload) (ZettelWriteResult, error) {
	data, err := json.Marshal(payload)
	if err != nil {
		return ZettelWriteResult{}, fmt.Errorf("marshal mission payload: %w", err)
	}

	args := []string{"zettel", "write", "mission"}
	if c.DroppedDir != "" {
		args = append(args, "--dropped-dir", c.DroppedDir)
	}

	vaultPath := os.Getenv("OBSIDIAN_VAULT_PATH")
	if vaultPath == "" {
		vaultPath = filepath.Join(os.Getenv("HOME"), ".alluka", "vault")
	}

	cmd := exec.CommandContext(ctx, c.BinPath, args...)
	cmd.Stdin = bytes.NewReader(data)
	cmd.Env = append(os.Environ(), "OBSIDIAN_VAULT_PATH="+vaultPath)

	out, err := cmd.Output()
	if err != nil {
		return ZettelWriteResult{}, fmt.Errorf("obsidian zettel write: %w", err)
	}

	var result ZettelWriteResult
	if err := json.Unmarshal(out, &result); err != nil {
		return ZettelWriteResult{}, fmt.Errorf("parse obsidian output: %w", err)
	}

	return result, nil
}

// ZettelHook subscribes to MissionCompleted events and writes mission Zettels
// by delegating to a ZettelCLI implementation.
type ZettelHook struct {
	cli     ZettelCLI
	emitter event.Emitter
}

// NewZettelHook creates a ZettelHook that shells out to the obsidian binary at
// binPath. droppedDir is forwarded to the binary for fallback staging when the
// vault is read-only.
func NewZettelHook(binPath, droppedDir string, emitter event.Emitter) *ZettelHook {
	return &ZettelHook{
		cli:     &ExecZettelCLI{BinPath: binPath, DroppedDir: droppedDir},
		emitter: emitter,
	}
}

// OnMissionComplete handles a single MissionCompleted event: it builds the
// mission payload from event data, calls the CLI, and emits ZettelWritten,
// ZettelSkipped, or ZettelWriteFailed depending on the outcome.
func (h *ZettelHook) OnMissionComplete(ctx context.Context, ev event.Event) {
	payload := MissionPayload{
		ID:        ev.MissionID,
		Slug:      stringFromData(ev.Data, "slug"),
		Completed: ev.Timestamp,
		Personas:  stringSliceFromData(ev.Data, "personas"),
		Trackers:  stringSliceFromData(ev.Data, "trackers"),
		Artifacts: stringSliceFromData(ev.Data, "artifacts_list"),
		IdeaSlug:  stringFromData(ev.Data, "idea_slug"),
	}

	result, err := h.cli.WriteMission(ctx, payload)
	if err != nil {
		h.emitter.Emit(ctx, event.New(event.ZettelWriteFailed, ev.MissionID, "", "", map[string]any{
			"slug":         payload.Slug,
			"reason":       err.Error(),
			"dropped_path": "",
		}))
		return
	}

	if result.Dropped {
		h.emitter.Emit(ctx, event.New(event.ZettelWriteFailed, ev.MissionID, "", "", map[string]any{
			"slug":         payload.Slug,
			"reason":       result.Error,
			"dropped_path": result.DroppedPath,
			"warnings":     result.Warnings,
		}))
		return
	}

	if result.Skipped {
		h.emitter.Emit(ctx, event.New(event.ZettelSkipped, ev.MissionID, "", "", map[string]any{
			"path":     result.Path,
			"reason":   result.SkipReason,
			"warnings": result.Warnings,
		}))
		return
	}

	h.emitter.Emit(ctx, event.New(event.ZettelWritten, ev.MissionID, "", "", map[string]any{
		"path":     result.Path,
		"type":     "mission",
		"warnings": result.Warnings,
	}))
}

// stringFromData extracts a string value from an event data map.
func stringFromData(data map[string]any, key string) string {
	v, _ := data[key].(string)
	return v
}

// stringSliceFromData extracts a string slice from an event data map,
// handling both []string and []any (the latter produced by JSON round-trips).
func stringSliceFromData(data map[string]any, key string) []string {
	v, ok := data[key]
	if !ok {
		return nil
	}
	switch t := v.(type) {
	case []string:
		return t
	case []any:
		out := make([]string, 0, len(t))
		for _, item := range t {
			if s, ok := item.(string); ok {
				out = append(out, s)
			}
		}
		return out
	}
	return nil
}
