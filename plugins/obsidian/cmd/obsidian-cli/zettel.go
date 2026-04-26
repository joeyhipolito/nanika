// zettel.go — RFC §7 Phase 1 (TRK-525): `zettel write mission` subcommand.
// Reads a MissionPayload JSON from stdin, writes the rendered Zettel via the
// zettel library, and prints a ZettelWriteOutput JSON to stdout.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/zettel"
)

// zettelMissionPayload is the JSON body expected on stdin for `zettel write mission`.
type zettelMissionPayload struct {
	ID        string    `json:"id"`
	Slug      string    `json:"slug"`
	Completed time.Time `json:"completed"`
	Personas  []string  `json:"personas"`
	Trackers  []string  `json:"trackers"`
	Artifacts []string  `json:"artifacts"`
	IdeaSlug  string    `json:"idea_slug,omitempty"`
}

// zettelWriteOutput is the JSON response written to stdout.
type zettelWriteOutput struct {
	Path        string   `json:"path"`
	Skipped     bool     `json:"skipped"`
	SkipReason  string   `json:"skip_reason,omitempty"`
	Dropped     bool     `json:"dropped,omitempty"`
	DroppedPath string   `json:"dropped_path,omitempty"`
	Warnings    []string `json:"warnings,omitempty"`
	Error       string   `json:"error,omitempty"`
}

// handleZettelCommand dispatches `zettel` subcommands.
func handleZettelCommand(vaultPath string, args []string) error {
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		fmt.Print(`Usage: obsidian zettel <subcommand>

Subcommands:
  write mission   Read MissionPayload JSON from stdin and write a mission Zettel

Options:
  --dropped-dir <path>   Fallback directory for failed writes (used with write)
  --help, -h             Show this help
`)
		return nil
	}

	switch args[0] {
	case "write":
		if len(args) < 2 {
			return fmt.Errorf("%w: zettel write requires a type: write mission", errUsage)
		}
		writeType := args[1]
		writeArgs := args[2:]
		switch writeType {
		case "mission":
			return handleZettelWriteMission(vaultPath, writeArgs)
		default:
			return fmt.Errorf("%w: unknown zettel write type: %s", errUsage, writeType)
		}
	default:
		return fmt.Errorf("%w: unknown zettel subcommand: %s", errUsage, args[0])
	}
}

// handleZettelWriteMission reads a zettelMissionPayload from stdin, writes the
// mission Zettel via the zettel library, and encodes the result to stdout.
// On write failure it attempts to stage the rendered content in --dropped-dir.
func handleZettelWriteMission(vaultPath string, args []string) error {
	droppedDir := ""
	for i := 0; i < len(args); i++ {
		if args[i] == "--dropped-dir" && i+1 < len(args) {
			droppedDir = args[i+1]
			i++
		}
	}

	var payload zettelMissionPayload
	if err := json.NewDecoder(os.Stdin).Decode(&payload); err != nil {
		return zettelWriteJSON(zettelWriteOutput{Error: "invalid JSON payload: " + err.Error()})
	}

	m := zettel.Mission{
		ID:        payload.ID,
		Slug:      payload.Slug,
		Completed: payload.Completed,
		Personas:  payload.Personas,
		Trackers:  payload.Trackers,
		Artifacts: payload.Artifacts,
	}

	w, err := zettel.NewWriter(vaultPath)
	if err != nil {
		return zettelTryDrop(droppedDir, payload, m, "cannot open writer: "+err.Error())
	}
	defer w.Dedup.Close()

	result, err := w.WriteMission(payload.ID, payload.Slug, payload.IdeaSlug, m)
	if err != nil {
		return zettelTryDrop(droppedDir, payload, m, err.Error())
	}

	return zettelWriteJSON(zettelWriteOutput{
		Path:       result.Path,
		Skipped:    result.Skipped,
		SkipReason: result.SkipReason,
		Warnings:   result.Warnings,
	})
}

// zettelTryDrop stages the rendered Zettel in droppedDir when the vault write
// fails, then writes the outcome JSON to stdout.
func zettelTryDrop(droppedDir string, payload zettelMissionPayload, m zettel.Mission, reason string) error {
	out := zettelWriteOutput{Error: reason}

	if droppedDir != "" {
		ts := time.Now().UTC().Format("20060102T150405")
		slug := payload.Slug
		if slug == "" {
			slug = payload.ID
		}
		droppedPath := filepath.Join(droppedDir, ts+"-"+slug+".md")
		if err := os.MkdirAll(droppedDir, 0755); err == nil {
			content := zettel.RenderMission(m)
			if writeErr := os.WriteFile(droppedPath, []byte(content), 0644); writeErr == nil {
				out.Dropped = true
				out.DroppedPath = droppedPath
			}
		}
	}

	return zettelWriteJSON(out)
}

// zettelWriteJSON encodes v as JSON to stdout with a trailing newline.
func zettelWriteJSON(v any) error {
	return json.NewEncoder(os.Stdout).Encode(v)
}
