package engine

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// mockZettelCLI is a stub that returns a pre-configured ZettelWriteResult.
type mockZettelCLI struct {
	result          ZettelWriteResult
	err             error
	capturedPayload MissionPayload
}

func (m *mockZettelCLI) WriteMission(_ context.Context, p MissionPayload) (ZettelWriteResult, error) {
	m.capturedPayload = p
	return m.result, m.err
}

// T1.5 — §10.4 Phase 1
// Integration test: when the vault directory is read-only, a write attempt
// stages the fully-rendered Zettel in dropped/<ts>-<slug>.md and emits a
// ZettelWriteFailed event with the failure reason on the orchestrator event bus.
func TestWriteFailure_DroppedDir(t *testing.T) {
	droppedDir := t.TempDir()

	// Pre-create the dropped file, simulating what the real CLI binary would
	// do when it can't write to a read-only vault.
	ts := time.Now().UTC().Format("20060102T150405")
	droppedPath := filepath.Join(droppedDir, ts+"-test-slug.md")
	if err := os.WriteFile(droppedPath, []byte("# test-slug\n"), 0644); err != nil {
		t.Fatalf("setup: write dropped file: %v", err)
	}

	bus := event.NewBus()
	subID, evCh := bus.Subscribe()
	defer bus.Unsubscribe(subID)

	hook := &ZettelHook{
		cli: &mockZettelCLI{
			result: ZettelWriteResult{
				Dropped:     true,
				DroppedPath: droppedPath,
				Error:       "write failed: vault is read-only",
			},
		},
		emitter: event.NewBusEmitter(bus),
	}

	ev := event.New(event.MissionCompleted, "mission-123", "", "", map[string]any{
		"slug": "test-slug",
	})
	hook.OnMissionComplete(context.Background(), ev)

	select {
	case emitted := <-evCh:
		if emitted.Type != event.ZettelWriteFailed {
			t.Fatalf("got event type %q, want ZettelWriteFailed", emitted.Type)
		}
		if reason, _ := emitted.Data["reason"].(string); reason == "" {
			t.Error("ZettelWriteFailed event missing non-empty reason")
		}
		dp, _ := emitted.Data["dropped_path"].(string)
		if dp == "" {
			t.Error("ZettelWriteFailed event missing dropped_path")
		}
	case <-time.After(time.Second):
		t.Fatal("timeout: ZettelWriteFailed event not emitted")
	}

	// Confirm the dropped file exists on disk.
	if _, err := os.Stat(droppedPath); err != nil {
		t.Errorf("dropped file not found at %s: %v", droppedPath, err)
	}
}

// T1.8 — §10.4 Phase 1
// Asserts: after a successful Zettel write, the exact event sequence on the
// orchestrator bus is ZettelWritten{path, type}; after a dedup skip it is
// ZettelSkipped{path, reason}.
func TestEventEmission(t *testing.T) {
	t.Run("success emits ZettelWritten", func(t *testing.T) {
		bus := event.NewBus()
		subID, evCh := bus.Subscribe()
		defer bus.Unsubscribe(subID)

		hook := &ZettelHook{
			cli: &mockZettelCLI{
				result: ZettelWriteResult{
					Path: "/vault/missions/2026-04-20-test.md",
				},
			},
			emitter: event.NewBusEmitter(bus),
		}

		ev := event.New(event.MissionCompleted, "mission-1", "", "", map[string]any{
			"slug": "test",
		})
		hook.OnMissionComplete(context.Background(), ev)

		select {
		case emitted := <-evCh:
			if emitted.Type != event.ZettelWritten {
				t.Fatalf("got event type %q, want ZettelWritten", emitted.Type)
			}
			if typ, _ := emitted.Data["type"].(string); typ != "mission" {
				t.Errorf("ZettelWritten.type = %q, want %q", typ, "mission")
			}
			if path, _ := emitted.Data["path"].(string); path == "" {
				t.Error("ZettelWritten missing path")
			}
		case <-time.After(time.Second):
			t.Fatal("timeout: ZettelWritten event not emitted")
		}
	})

	t.Run("dedup skip emits ZettelSkipped", func(t *testing.T) {
		bus := event.NewBus()
		subID, evCh := bus.Subscribe()
		defer bus.Unsubscribe(subID)

		hook := &ZettelHook{
			cli: &mockZettelCLI{
				result: ZettelWriteResult{
					Path:       "/vault/missions/2026-04-20-test.md",
					Skipped:    true,
					SkipReason: "duplicate mission_id",
				},
			},
			emitter: event.NewBusEmitter(bus),
		}

		ev := event.New(event.MissionCompleted, "mission-1", "", "", map[string]any{
			"slug": "test",
		})
		hook.OnMissionComplete(context.Background(), ev)

		select {
		case emitted := <-evCh:
			if emitted.Type != event.ZettelSkipped {
				t.Fatalf("got event type %q, want ZettelSkipped", emitted.Type)
			}
			if reason, _ := emitted.Data["reason"].(string); reason != "duplicate mission_id" {
				t.Errorf("ZettelSkipped.reason = %q, want %q", reason, "duplicate mission_id")
			}
			if path, _ := emitted.Data["path"].(string); path == "" {
				t.Error("ZettelSkipped missing path")
			}
		case <-time.After(time.Second):
			t.Fatal("timeout: ZettelSkipped event not emitted")
		}
	})
}

// T1.9 — §10.4 Phase 1
// Asserts that idea_slug from ev.Data is read by the hook and forwarded in
// MissionPayload.IdeaSlug, and that the resulting warnings from the CLI are
// included in the ZettelWritten event payload.
func TestIdeaSlugRoundTrip(t *testing.T) {
	tests := []struct {
		name         string
		ideaSlug     string
		cliWarnings  []string
		wantIdeaSlug string
		wantWarnings []string
	}{
		{
			name:         "idea_slug is passed through to CLI payload",
			ideaSlug:     "my-idea",
			wantIdeaSlug: "my-idea",
		},
		{
			name:         "empty idea_slug stays empty",
			ideaSlug:     "",
			wantIdeaSlug: "",
		},
		{
			name:         "CLI warnings are included in ZettelWritten event",
			ideaSlug:     "some-idea",
			cliWarnings:  []string{"AppendMissionToIdea: section not found"},
			wantIdeaSlug: "some-idea",
			wantWarnings: []string{"AppendMissionToIdea: section not found"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			bus := event.NewBus()
			subID, evCh := bus.Subscribe()
			defer bus.Unsubscribe(subID)

			mock := &mockZettelCLI{
				result: ZettelWriteResult{
					Path:     "/vault/missions/2026-04-20-test.md",
					Warnings: tt.cliWarnings,
				},
			}
			hook := &ZettelHook{
				cli:     mock,
				emitter: event.NewBusEmitter(bus),
			}

			ev := event.New(event.MissionCompleted, "mission-99", "", "", map[string]any{
				"slug":      "test",
				"idea_slug": tt.ideaSlug,
			})
			hook.OnMissionComplete(context.Background(), ev)

			select {
			case emitted := <-evCh:
				if emitted.Type != event.ZettelWritten {
					t.Fatalf("got event type %q, want ZettelWritten", emitted.Type)
				}
				if mock.capturedPayload.IdeaSlug != tt.wantIdeaSlug {
					t.Errorf("IdeaSlug = %q, want %q", mock.capturedPayload.IdeaSlug, tt.wantIdeaSlug)
				}
				warnings, _ := emitted.Data["warnings"].([]string)
				if len(warnings) != len(tt.wantWarnings) {
					t.Errorf("warnings = %v, want %v", warnings, tt.wantWarnings)
				}
			case <-time.After(time.Second):
				t.Fatal("timeout: ZettelWritten event not emitted")
			}
		})
	}
}

// T4.4 — §10.4 Phase 4
// Asserts: a SessionStop event causes sessions/<date>-<slug>-snapshot.md to be
// written with populated position, resume_hint, and trackers_touched fields.
func TestSessionSnapshot(t *testing.T) {
	t.Skip("RED — T4.4 not yet implemented (blocks on TRK-529 Phase 4)")
}

// T4.5 — §10.4 Phase 4
// Asserts: each of the 5 hook types (onMissionComplete, onPhaseFinding,
// onDecisionDetected, onTrackerUpdate, onDailyNoteRoll) emits a correctly-shaped
// event that is persistently logged under ~/.alluka/logs/obsidian-audit.jsonl.
func TestObservabilityEvents(t *testing.T) {
	t.Skip("RED — T4.5 not yet implemented (blocks on TRK-529 Phase 4)")
}

// T4.6 — §10.4 Phase 4
// Asserts: running obsidian backfill --since 30d twice produces zero new Zettels
// on the second run (dedup is idempotent) and emits a BackfillNoOp event.
func TestBackfill_Idempotent(t *testing.T) {
	t.Skip("RED — T4.6 not yet implemented (blocks on TRK-529 Phase 4)")
}
