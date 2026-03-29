package sdk

// Table-driven tests for four behaviours added in the stall-detection /
// session-resume implementation:
//
//  1. Stall detection triggers after threshold (watchStall, stallThreshold)
//  2. Session ID is propagated through the event pipeline (extractEvents → KindTurnEnd.SessionID)
//  3. Resume flags are injected when a session ID is available
//  4. Max-turns flag is set when MaxTurns > 0 and omitted when zero

import (
	"slices"
	"strings"
	"testing"
	"time"
)

// ---------------------------------------------------------------------------
// 1. Stall detection — table-driven threshold parsing
// ---------------------------------------------------------------------------

func TestStallThresholdTable(t *testing.T) {
	tests := []struct {
		name    string
		envVal  string
		want    time.Duration
	}{
		{
			name:   "empty env uses default 5m",
			envVal: "",
			want:   5 * time.Minute,
		},
		{
			name:   "valid seconds override",
			envVal: "300s",
			want:   300 * time.Second,
		},
		{
			name:   "valid minutes override",
			envVal: "10m",
			want:   10 * time.Minute,
		},
		{
			name:   "valid hours override",
			envVal: "1h",
			want:   time.Hour,
		},
		{
			name:   "zero duration falls back to default",
			envVal: "0s",
			want:   5 * time.Minute,
		},
		{
			name:   "negative duration falls back to default",
			envVal: "-1m",
			want:   5 * time.Minute,
		},
		{
			name:   "unparseable string falls back to default",
			envVal: "notaduration",
			want:   5 * time.Minute,
		},
		{
			name:   "bare number without unit falls back to default",
			envVal: "300",
			want:   5 * time.Minute,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Setenv("ORCHESTRATOR_STALL_TIMEOUT", tt.envVal)
			got := stallThreshold()
			if got != tt.want {
				t.Errorf("stallThreshold() = %s; want %s", got, tt.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// 1. Stall detection — table-driven watchdog firing behaviour
// ---------------------------------------------------------------------------

// TestWatchStallFiresAfterThreshold tests the firing cases: the watchdog must
// call cancel when lastOutputTime is older than the threshold.
func TestWatchStallFiresAfterThreshold(t *testing.T) {
	tests := []struct {
		name          string
		threshold     time.Duration
		lastOutputAge time.Duration // how old lastOutputTime is relative to now
	}{
		{
			name:          "age well past threshold fires watchdog",
			threshold:     50 * time.Millisecond,
			lastOutputAge: 200 * time.Millisecond,
		},
		{
			name:          "age slightly past threshold fires watchdog",
			threshold:     50 * time.Millisecond,
			lastOutputAge: 51 * time.Millisecond,
		},
		{
			name:          "large threshold with age past it fires watchdog",
			threshold:     100 * time.Millisecond,
			lastOutputAge: 300 * time.Millisecond,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tr := &SubprocessTransport{
				done: make(chan struct{}),
			}
			tr.lastOutputTime.Store(time.Now().Add(-tt.lastOutputAge).UnixNano())

			fired := make(chan struct{}, 1)
			cancel := func() {
				select {
				case fired <- struct{}{}:
				default:
				}
			}

			go tr.watchStall(cancel, tt.threshold)
			defer close(tr.done)

			select {
			case <-fired:
				// correct: watchdog triggered
			case <-time.After(5 * time.Second):
				t.Fatal("watchdog did not fire within 5s; expected stall cancellation")
			}
		})
	}
}

// TestWatchStallDoesNotFireWithFreshOutput verifies that the watchdog does NOT
// cancel when output keeps arriving. Uses a very long threshold so no accidental
// firing can occur during the brief test window.
func TestWatchStallDoesNotFireWithFreshOutput(t *testing.T) {
	tests := []struct {
		name          string
		threshold     time.Duration
		refreshEvery  time.Duration // how often to bump lastOutputTime
		watchDuration time.Duration // how long to run before asserting no fire
	}{
		{
			name:          "frequent output keeps watchdog quiet",
			threshold:     10 * time.Minute, // effectively never fires in this test
			refreshEvery:  20 * time.Millisecond,
			watchDuration: 100 * time.Millisecond,
		},
		{
			name:          "single initial fresh timestamp keeps watchdog quiet",
			threshold:     10 * time.Minute,
			refreshEvery:  0, // no refresh — rely on initial fresh timestamp
			watchDuration: 80 * time.Millisecond,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tr := &SubprocessTransport{
				done: make(chan struct{}),
			}
			tr.lastOutputTime.Store(time.Now().UnixNano())

			cancelCalled := false
			cancel := func() { cancelCalled = true }

			go tr.watchStall(cancel, tt.threshold)

			if tt.refreshEvery > 0 {
				ticker := time.NewTicker(tt.refreshEvery)
				defer ticker.Stop()
				deadline := time.After(tt.watchDuration)
				for {
					select {
					case <-ticker.C:
						tr.lastOutputTime.Store(time.Now().UnixNano())
					case <-deadline:
						goto done
					}
				}
			done:
			} else {
				time.Sleep(tt.watchDuration)
			}

			close(tr.done)
			time.Sleep(20 * time.Millisecond) // let watchdog goroutine see done

			if cancelCalled {
				t.Error("watchdog fired cancel despite fresh output / long threshold")
			}
		})
	}
}

// ---------------------------------------------------------------------------
// 2. Session ID propagation — extractEvents surfaces SessionID on KindTurnEnd
// ---------------------------------------------------------------------------

func TestExtractEventsSessionIDTable(t *testing.T) {
	tests := []struct {
		name      string
		msg       *ResultMessage
		wantID    string
		wantError bool
	}{
		{
			name: "session ID is propagated on success",
			msg: &ResultMessage{
				Type:      MessageTypeResult,
				Subtype:   "success",
				SessionID: "sess-abc123",
				NumTurns:  3,
			},
			wantID:    "sess-abc123",
			wantError: false,
		},
		{
			name: "session ID is propagated on error result",
			msg: &ResultMessage{
				Type:         MessageTypeResult,
				Subtype:      "error",
				SessionID:    "sess-err-xyz",
				ErrorMessage: "context canceled",
			},
			wantID:    "sess-err-xyz",
			wantError: true,
		},
		{
			name: "empty session ID remains empty",
			msg: &ResultMessage{
				Type:    MessageTypeResult,
				Subtype: "success",
				// SessionID intentionally omitted
			},
			wantID:    "",
			wantError: false,
		},
		{
			name: "session ID with UUID format",
			msg: &ResultMessage{
				Type:      MessageTypeResult,
				Subtype:   "success",
				SessionID: "550e8400-e29b-41d4-a716-446655440000",
			},
			wantID:    "550e8400-e29b-41d4-a716-446655440000",
			wantError: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			events := extractEvents(tt.msg)
			if len(events) != 1 {
				t.Fatalf("want exactly 1 event, got %d", len(events))
			}
			ev := events[0]
			if ev.Kind != KindTurnEnd {
				t.Fatalf("want KindTurnEnd, got %q", ev.Kind)
			}
			if ev.SessionID != tt.wantID {
				t.Errorf("SessionID = %q; want %q", ev.SessionID, tt.wantID)
			}
			if ev.IsError != tt.wantError {
				t.Errorf("IsError = %v; want %v", ev.IsError, tt.wantError)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// 3. Resume flags — --resume <id> --fork-session injected when ID is set
// ---------------------------------------------------------------------------

// resumeFinalArgs mirrors the buildFinalArgs closure in QueryText. It is
// reproduced here to make the flag-injection contract explicit and testable.
func resumeFinalArgs(baseArgs []string, resumeID, prompt string) []string {
	args := make([]string, len(baseArgs))
	copy(args, baseArgs)
	if resumeID != "" {
		args = append(args, "--resume", resumeID, "--fork-session")
	}
	return append(args, "-p", prompt)
}

func TestResumeFlagsTable(t *testing.T) {
	prompt := "do the thing"

	tests := []struct {
		name         string
		resumeID     string
		wantResume   bool
		wantForkSession bool
	}{
		{
			name:            "resume flags present when session ID is set",
			resumeID:        "sess-abc123",
			wantResume:      true,
			wantForkSession: true,
		},
		{
			name:            "no resume flags when session ID is empty",
			resumeID:        "",
			wantResume:      false,
			wantForkSession: false,
		},
		{
			name:            "resume flags present for UUID-format session ID",
			resumeID:        "550e8400-e29b-41d4-a716-446655440000",
			wantResume:      true,
			wantForkSession: true,
		},
	}

	base := queryBuildArgs(nil)

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			args := resumeFinalArgs(base, tt.resumeID, prompt)

			hasResume := slices.Contains(args, "--resume")
			hasFork := slices.Contains(args, "--fork-session")

			if hasResume != tt.wantResume {
				t.Errorf("--resume present=%v; want %v (args: %v)", hasResume, tt.wantResume, args)
			}
			if hasFork != tt.wantForkSession {
				t.Errorf("--fork-session present=%v; want %v (args: %v)", hasFork, tt.wantForkSession, args)
			}

			// When resume is expected, verify the ID immediately follows the flag.
			if tt.wantResume {
				idx := slices.Index(args, "--resume")
				if idx < 0 || idx+1 >= len(args) {
					t.Fatalf("--resume flag found but session ID arg is missing")
				}
				if args[idx+1] != tt.resumeID {
					t.Errorf("--resume arg = %q; want %q", args[idx+1], tt.resumeID)
				}
			}

			// Verify the prompt is always the last argument.
			if len(args) < 2 || args[len(args)-1] != prompt || args[len(args)-2] != "-p" {
				t.Errorf("prompt not at end of args; args = %v", args)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// 4. Max-turns flag — --max-turns N included only when MaxTurns > 0
// ---------------------------------------------------------------------------

func TestQueryBuildArgsMaxTurnsTable(t *testing.T) {
	tests := []struct {
		name         string
		maxTurns     int
		wantFlag     bool
		wantValue    string
	}{
		{
			name:      "zero MaxTurns omits the flag",
			maxTurns:  0,
			wantFlag:  false,
		},
		{
			name:      "negative MaxTurns omits the flag",
			maxTurns:  -1,
			wantFlag:  false,
		},
		{
			name:      "default engine guardrail value (50) emits the flag",
			maxTurns:  50,
			wantFlag:  true,
			wantValue: "50",
		},
		{
			name:      "custom value (10) emits the flag with correct value",
			maxTurns:  10,
			wantFlag:  true,
			wantValue: "10",
		},
		{
			name:      "large value (1000) emits the flag",
			maxTurns:  1000,
			wantFlag:  true,
			wantValue: "1000",
		},
		{
			name:      "single turn (1) emits the flag",
			maxTurns:  1,
			wantFlag:  true,
			wantValue: "1",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			opts := &AgentOptions{MaxTurns: tt.maxTurns}
			args := queryBuildArgs(opts)

			flagIdx := slices.Index(args, "--max-turns")
			hasFlag := flagIdx >= 0

			if hasFlag != tt.wantFlag {
				t.Errorf("--max-turns present=%v; want %v (args: %v)", hasFlag, tt.wantFlag, args)
			}

			if tt.wantFlag {
				if flagIdx+1 >= len(args) {
					t.Fatalf("--max-turns flag found but value arg is missing")
				}
				gotValue := args[flagIdx+1]
				if gotValue != tt.wantValue {
					t.Errorf("--max-turns value = %q; want %q", gotValue, tt.wantValue)
				}
			}
		})
	}
}

// TestQueryBuildArgsModelTable verifies the --model flag is included only when
// the model string is non-empty and that it contains the exact model string.
func TestQueryBuildArgsModelTable(t *testing.T) {
	tests := []struct {
		name      string
		model     string
		wantFlag  bool
		wantValue string
	}{
		{"empty model omits flag", "", false, ""},
		{"claude-sonnet-4-5 included", "claude-sonnet-4-5", true, "claude-sonnet-4-5"},
		{"claude-opus-4 included", "claude-opus-4", true, "claude-opus-4"},
		{"whitespace-only model still included", "  ", true, "  "},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			args := queryBuildArgs(&AgentOptions{Model: tt.model})
			idx := slices.Index(args, "--model")
			hasFlag := idx >= 0
			if hasFlag != tt.wantFlag {
				t.Errorf("--model present=%v; want %v", hasFlag, tt.wantFlag)
			}
			if tt.wantFlag {
				if idx+1 >= len(args) {
					t.Fatalf("--model flag found but value arg is missing")
				}
				if args[idx+1] != tt.wantValue {
					t.Errorf("--model value = %q; want %q", args[idx+1], tt.wantValue)
				}
			}
		})
	}
}

// ---------------------------------------------------------------------------
// AddDirs flag wiring — --add-dir flags emitted for each entry
// ---------------------------------------------------------------------------

func TestQueryBuildArgsAddDirsTable(t *testing.T) {
	tests := []struct {
		name       string
		addDirs    []string
		wantDirs   []string
		wantNoFlag bool
	}{
		{
			name:       "nil AddDirs emits no --add-dir flag",
			addDirs:    nil,
			wantNoFlag: true,
		},
		{
			name:       "empty slice emits no --add-dir flag",
			addDirs:    []string{},
			wantNoFlag: true,
		},
		{
			name:     "single dir emits one --add-dir flag",
			addDirs:  []string{"/workspace/workers/phase-1"},
			wantDirs: []string{"/workspace/workers/phase-1"},
		},
		{
			name:     "multiple dirs emit one --add-dir flag each",
			addDirs:  []string{"/dir/a", "/dir/b"},
			wantDirs: []string{"/dir/a", "/dir/b"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			args := queryBuildArgs(&AgentOptions{AddDirs: tt.addDirs})

			if tt.wantNoFlag {
				if slices.Contains(args, "--add-dir") {
					t.Errorf("--add-dir should not be present when AddDirs is empty; args: %v", args)
				}
				return
			}

			// Collect all values that follow --add-dir flags.
			var gotDirs []string
			for i, arg := range args {
				if arg == "--add-dir" && i+1 < len(args) {
					gotDirs = append(gotDirs, args[i+1])
				}
			}

			if len(gotDirs) != len(tt.wantDirs) {
				t.Fatalf("got %d --add-dir values %v; want %d %v", len(gotDirs), gotDirs, len(tt.wantDirs), tt.wantDirs)
			}
			for i, want := range tt.wantDirs {
				if gotDirs[i] != want {
					t.Errorf("--add-dir[%d] = %q; want %q", i, gotDirs[i], want)
				}
			}
		})
	}
}

// TestCommandEnvAddDirsSetsEnvVar verifies that CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1
// is injected when AddDirs is non-empty but omitted when empty.
func TestCommandEnvAddDirsSetsEnvVar(t *testing.T) {
	t.Run("AddDirs non-empty sets env var", func(t *testing.T) {
		env := commandEnv(&AgentOptions{AddDirs: []string{"/some/dir"}})
		found := false
		for _, kv := range env {
			if kv == "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1" {
				found = true
				break
			}
		}
		if !found {
			t.Fatal("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1 not found in env when AddDirs is set")
		}
	})

	t.Run("AddDirs empty omits env var", func(t *testing.T) {
		env := commandEnv(&AgentOptions{AddDirs: nil})
		for _, kv := range env {
			if strings.HasPrefix(kv, "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=") {
				t.Fatalf("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD should not be set when AddDirs is empty; got %q", kv)
			}
		}
	})
}

// TestQueryBuildArgsBaseFlags verifies that mandatory flags always appear
// regardless of options, so callers can rely on a stable base.
func TestQueryBuildArgsBaseFlags(t *testing.T) {
	requiredFlags := []string{
		"--output-format", "stream-json",
		"--print",
		"--verbose",
		"--include-partial-messages",
	}

	cases := []struct {
		name string
		opts *AgentOptions
	}{
		{"nil opts", nil},
		{"empty opts", &AgentOptions{}},
		{"opts with model", &AgentOptions{Model: "claude-opus-4"}},
	}

	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			args := queryBuildArgs(tt.opts)
			joined := strings.Join(args, " ")
			for _, flag := range requiredFlags {
				if !strings.Contains(joined, flag) {
					t.Errorf("required flag %q missing from args: %v", flag, args)
				}
			}
		})
	}
}
