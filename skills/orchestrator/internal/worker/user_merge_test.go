package worker

import (
	"context"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika/shared/sdk"
)

// withFakeNudge swaps the nudge LLM seam and the sampler for a deterministic
// test. The supplied fn is called when the LLM is invoked; pass nil to assert
// the LLM is never called.
func withFakeNudge(t *testing.T, sample float64, fn func(ctx context.Context, prompt string, opts *sdk.AgentOptions) (string, error)) {
	t.Helper()
	prevText := nudgeQueryText
	prevRand := nudgeRand
	called := false
	nudgeRand = func() float64 { return sample }
	nudgeQueryText = func(ctx context.Context, prompt string, opts *sdk.AgentOptions) (string, error) {
		called = true
		if fn == nil {
			t.Fatalf("nudge LLM unexpectedly invoked: prompt len=%d", len(prompt))
		}
		return fn(ctx, prompt, opts)
	}
	t.Cleanup(func() {
		nudgeQueryText = prevText
		nudgeRand = prevRand
		if fn == nil && called {
			t.Errorf("nudge LLM was called but the test expected no call")
		}
	})
}

// writeUserProfileFor stages a USER.md inside a fake $HOME so LoadUserProfile
// reads it. Returns the project dir to pass into the nudge.
func writeUserProfileFor(t *testing.T, profile *UserProfile) string {
	t.Helper()
	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, "project")
	if err := os.MkdirAll(projectDir, 0o700); err != nil {
		t.Fatalf("mkdir project: %v", err)
	}
	if err := SaveUserProfile(projectDir, profile); err != nil {
		t.Fatalf("seed USER.md: %v", err)
	}
	return projectDir
}

// writeWorkspaceWithOutput creates a minimal workspace dir containing a single
// worker output.md so readWorkerOutputs has something to return.
func writeWorkspaceWithOutput(t *testing.T, body string) string {
	t.Helper()
	ws := t.TempDir()
	dir := filepath.Join(ws, "workers", "phase-1")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatalf("mkdir worker: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte(body), 0o600); err != nil {
		t.Fatalf("write output.md: %v", err)
	}
	return ws
}

func TestPeriodicNudge_GatedByEnv(t *testing.T) {
	t.Setenv("NANIKA_USER_PROFILE_NUDGE", "")
	withFakeNudge(t, 0.0, nil) // any LLM call would fail the test

	projectDir := writeUserProfileFor(t, &UserProfile{
		Name:                 "Tester",
		OptedInForAutoUpdate: true,
	})
	ws := writeWorkspaceWithOutput(t, strings.Repeat("hello world. ", 100))

	if err := PeriodicallyNudgeUserProfile(context.Background(), projectDir, ws, "dev"); err != nil {
		t.Fatalf("nudge: %v", err)
	}
}

func TestPeriodicNudge_GatedBySampler(t *testing.T) {
	t.Setenv("NANIKA_USER_PROFILE_NUDGE", "1")
	withFakeNudge(t, 0.5, nil) // > 0.1 → sample misses, no LLM call

	projectDir := writeUserProfileFor(t, &UserProfile{
		Name:                 "Tester",
		OptedInForAutoUpdate: true,
	})
	ws := writeWorkspaceWithOutput(t, strings.Repeat("hello world. ", 100))

	if err := PeriodicallyNudgeUserProfile(context.Background(), projectDir, ws, "dev"); err != nil {
		t.Fatalf("nudge: %v", err)
	}
}

func TestPeriodicNudge_GatedByOptIn(t *testing.T) {
	t.Setenv("NANIKA_USER_PROFILE_NUDGE", "1")
	withFakeNudge(t, 0.0, nil) // opt-in is false, no LLM call

	projectDir := writeUserProfileFor(t, &UserProfile{
		Name:                 "Tester",
		OptedInForAutoUpdate: false,
	})
	ws := writeWorkspaceWithOutput(t, strings.Repeat("hello world. ", 100))

	if err := PeriodicallyNudgeUserProfile(context.Background(), projectDir, ws, "dev"); err != nil {
		t.Fatalf("nudge: %v", err)
	}
}

func TestPeriodicNudge_FiresAndMerges(t *testing.T) {
	t.Setenv("NANIKA_USER_PROFILE_NUDGE", "1")

	type want struct {
		role        string
		preferences []string
	}

	cases := []struct {
		name      string
		response  string
		wantWrite bool
		want      want
	}{
		{
			name:      "fresh signals applied",
			response:  `{"role":"fullstack engineer","preferences":["wants terse responses"]}`,
			wantWrite: true,
			want: want{
				role:        "fullstack engineer",
				preferences: []string{"wants terse responses"},
			},
		},
		{
			name:      "fenced JSON gets stripped",
			response:  "```json\n{\"preferences\":[\"async communication\"]}\n```",
			wantWrite: true,
			want: want{
				role:        "",
				preferences: []string{"async communication"},
			},
		},
		{
			name:      "empty object is no-op",
			response:  `{}`,
			wantWrite: false,
		},
		{
			name:      "malformed prose is no-op",
			response:  `Sure, here are the signals: nothing actionable today.`,
			wantWrite: false,
		},
	}

	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			projectDir := writeUserProfileFor(t, &UserProfile{
				Name:                 "Tester",
				OptedInForAutoUpdate: true,
			})
			ws := writeWorkspaceWithOutput(t, strings.Repeat("transcript line. ", 50))

			var gotPrompt string
			var gotOpts *sdk.AgentOptions
			withFakeNudge(t, 0.05, func(_ context.Context, prompt string, opts *sdk.AgentOptions) (string, error) {
				gotPrompt = prompt
				gotOpts = opts
				return tt.response, nil
			})

			if err := PeriodicallyNudgeUserProfile(context.Background(), projectDir, ws, "dev"); err != nil {
				t.Fatalf("nudge: %v", err)
			}

			if gotOpts == nil {
				t.Fatal("LLM was not called")
			}
			if gotOpts.Model != "haiku" {
				t.Errorf("model: got %q, want haiku", gotOpts.Model)
			}
			if gotOpts.MaxTurns != 1 {
				t.Errorf("max_turns: got %d, want 1", gotOpts.MaxTurns)
			}
			if !strings.Contains(gotPrompt, "Existing user profile") {
				t.Errorf("prompt missing user profile anchor: %q", gotPrompt)
			}
			if !strings.Contains(gotPrompt, "Session transcript") {
				t.Errorf("prompt missing transcript anchor: %q", gotPrompt)
			}

			after, err := LoadUserProfile(projectDir)
			if err != nil {
				t.Fatalf("reload: %v", err)
			}
			if !after.OptedInForAutoUpdate {
				t.Error("opt-in flag was clobbered")
			}
			if after.Name != "Tester" {
				t.Errorf("Name clobbered: got %q", after.Name)
			}

			if !tt.wantWrite {
				if after.Role != "" || len(after.Preferences) > 0 {
					t.Errorf("expected no write but profile changed: role=%q prefs=%v", after.Role, after.Preferences)
				}
				return
			}
			if after.Role != tt.want.role {
				t.Errorf("Role: got %q, want %q", after.Role, tt.want.role)
			}
			if !reflect.DeepEqual(after.Preferences, tt.want.preferences) {
				t.Errorf("Preferences: got %v, want %v", after.Preferences, tt.want.preferences)
			}
		})
	}
}

func TestPeriodicNudge_LLMErrorPreservesProfile(t *testing.T) {
	t.Setenv("NANIKA_USER_PROFILE_NUDGE", "1")

	projectDir := writeUserProfileFor(t, &UserProfile{
		Name:                 "Tester",
		Role:                 "existing role",
		OptedInForAutoUpdate: true,
	})
	ws := writeWorkspaceWithOutput(t, strings.Repeat("transcript line. ", 50))

	withFakeNudge(t, 0.0, func(_ context.Context, _ string, _ *sdk.AgentOptions) (string, error) {
		return "", context.DeadlineExceeded
	})

	err := PeriodicallyNudgeUserProfile(context.Background(), projectDir, ws, "dev")
	if err == nil {
		t.Fatal("expected wrapped LLM error")
	}

	after, err := LoadUserProfile(projectDir)
	if err != nil {
		t.Fatalf("reload: %v", err)
	}
	if after.Role != "existing role" {
		t.Errorf("Role corrupted: got %q", after.Role)
	}
}

func TestPeriodicNudge_TranscriptTooShort(t *testing.T) {
	t.Setenv("NANIKA_USER_PROFILE_NUDGE", "1")
	withFakeNudge(t, 0.0, nil) // transcript too short → no LLM call

	projectDir := writeUserProfileFor(t, &UserProfile{
		Name:                 "Tester",
		OptedInForAutoUpdate: true,
	})
	ws := writeWorkspaceWithOutput(t, "tiny")

	if err := PeriodicallyNudgeUserProfile(context.Background(), projectDir, ws, "dev"); err != nil {
		t.Fatalf("nudge: %v", err)
	}
}

func TestParseNudgePatch(t *testing.T) {
	cases := []struct {
		name    string
		input   string
		want    *nudgePatch
		wantErr bool
	}{
		{
			name:  "plain object",
			input: `{"role":"x","preferences":["a","b"]}`,
			want:  &nudgePatch{Role: "x", Preferences: []string{"a", "b"}},
		},
		{
			name:  "fenced object",
			input: "```json\n{\"role\":\"x\"}\n```",
			want:  &nudgePatch{Role: "x"},
		},
		{
			name:  "unknown fields are ignored",
			input: `{"role":"x","mystery":42}`,
			want:  &nudgePatch{Role: "x"},
		},
		{
			name:  "empty object",
			input: `{}`,
			want:  &nudgePatch{},
		},
		{
			name:  "object surrounded by prose",
			input: "Here is the JSON: {\"role\":\"x\"} done.",
			want:  &nudgePatch{Role: "x"},
		},
		{
			name:    "empty string",
			input:   "",
			wantErr: true,
		},
		{
			name:    "pure prose",
			input:   "no JSON here, just prose",
			wantErr: true,
		},
	}

	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			got, err := parseNudgePatch(tt.input)
			if tt.wantErr {
				if err == nil {
					t.Fatalf("want error, got %+v", got)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if !reflect.DeepEqual(got, tt.want) {
				t.Errorf("got %+v, want %+v", got, tt.want)
			}
		})
	}
}

func TestMergeUserProfile(t *testing.T) {
	now := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

	cases := []struct {
		name       string
		existing   *UserProfile
		patch      *UserProfile
		wantRole   string
		wantPrefs  []string
		wantOptIn  bool
		wantName   string
		wantBumped bool // UpdatedAt must change
	}{
		{
			name:     "nil patch is a no-op",
			existing: &UserProfile{Name: "Tester", Role: "r", OptedInForAutoUpdate: true, UpdatedAt: now},
			patch:    nil,
			wantRole: "r", wantOptIn: true, wantName: "Tester",
		},
		{
			name:     "empty role does not clobber",
			existing: &UserProfile{Role: "kept", UpdatedAt: now},
			patch:    &UserProfile{Role: "", Preferences: []string{"x"}},
			wantRole: "kept",
			wantPrefs: []string{"x"},
			wantBumped: true,
		},
		{
			name:       "list union dedupes case-insensitively",
			existing:   &UserProfile{Preferences: []string{"Wants Terse Responses"}, UpdatedAt: now},
			patch:      &UserProfile{Preferences: []string{"wants terse responses", "async communication"}},
			wantPrefs:  []string{"Wants Terse Responses", "async communication"},
			wantBumped: true,
		},
		{
			name: "FIFO cap drops oldest existing items",
			existing: &UserProfile{
				Preferences: []string{
					"old1", "old2", "old3", "old4", "old5", "old6", "old7", "old8",
				},
				UpdatedAt: now,
			},
			patch:      &UserProfile{Preferences: []string{"new1", "new2"}},
			wantPrefs:  []string{"old3", "old4", "old5", "old6", "old7", "old8", "new1", "new2"},
			wantBumped: true,
		},
		{
			name:       "name and opt-in never touched",
			existing:   &UserProfile{Name: "Tester", OptedInForAutoUpdate: true, UpdatedAt: now},
			patch:      &UserProfile{Role: "patched"},
			wantRole:   "patched",
			wantName:   "Tester",
			wantOptIn:  true,
			wantBumped: true,
		},
		{
			name:     "no-op merge does not bump UpdatedAt",
			existing: &UserProfile{Role: "kept", Preferences: []string{"a"}, UpdatedAt: now},
			patch:    &UserProfile{Role: "kept", Preferences: []string{"a"}}, // duplicates
			wantRole: "kept", wantPrefs: []string{"a"},
		},
		{
			name:     "whitespace-only patch items dropped",
			existing: &UserProfile{Preferences: []string{"a"}, UpdatedAt: now},
			patch:    &UserProfile{Preferences: []string{"   ", ""}},
			wantPrefs: []string{"a"},
		},
	}

	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			got := MergeUserProfile(tt.existing, tt.patch)
			if got == nil {
				t.Fatal("nil result")
			}
			if got.Role != tt.wantRole {
				t.Errorf("Role: got %q, want %q", got.Role, tt.wantRole)
			}
			if tt.wantPrefs != nil && !reflect.DeepEqual(got.Preferences, tt.wantPrefs) {
				t.Errorf("Preferences: got %v, want %v", got.Preferences, tt.wantPrefs)
			}
			if got.OptedInForAutoUpdate != tt.wantOptIn {
				t.Errorf("OptedInForAutoUpdate: got %v, want %v", got.OptedInForAutoUpdate, tt.wantOptIn)
			}
			if got.Name != tt.wantName {
				t.Errorf("Name: got %q, want %q", got.Name, tt.wantName)
			}
			if tt.wantBumped {
				if !got.UpdatedAt.After(now) {
					t.Errorf("UpdatedAt should have been bumped past %v, got %v", now, got.UpdatedAt)
				}
			} else {
				if !got.UpdatedAt.Equal(now) {
					t.Errorf("UpdatedAt unexpectedly bumped: got %v want %v", got.UpdatedAt, now)
				}
			}
		})
	}
}

func TestMergeUserProfile_NilExisting(t *testing.T) {
	got := MergeUserProfile(nil, &UserProfile{Role: "r"})
	if got == nil || got.Role != "r" {
		t.Fatalf("got %+v", got)
	}
}

func TestCapItem(t *testing.T) {
	long := strings.Repeat("a", maxProfileItemLen+50)
	got := capItem(long)
	if len([]rune(got)) > maxProfileItemLen {
		t.Errorf("capItem did not enforce length: got %d", len([]rune(got)))
	}
}
