package sdk

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"
)

// TestFilteredEnv_Passthrough verifies that an out-of-allowlist variable is
// forwarded when PassthroughEnv=true and stripped when PassthroughEnv=false.
func TestFilteredEnv_Passthrough(t *testing.T) {
	const testVar = "SDK_TEST_PASSTHROUGH_VAR"
	const testVal = "sentinel-value"
	t.Setenv(testVar, testVal)

	tests := []struct {
		name        string
		passthrough bool
		wantPresent bool
	}{
		{
			name:        "passthrough true forwards unlisted variable",
			passthrough: true,
			wantPresent: true,
		},
		{
			name:        "passthrough false strips unlisted variable",
			passthrough: false,
			wantPresent: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			env := filteredEnv(nil, tt.passthrough)

			found := false
			for _, kv := range env {
				if strings.HasPrefix(kv, testVar+"=") {
					found = true
					break
				}
			}

			if found != tt.wantPresent {
				if tt.wantPresent {
					t.Errorf("%s should be present in env when PassthroughEnv=true, but it was absent", testVar)
				} else {
					t.Errorf("%s should be absent from env when PassthroughEnv=false, but it was present", testVar)
				}
			}
		})
	}
}

// TestContinueFlagInArgs verifies that --continue is appended to CLI args when
// ContinueConversation=true, and is absent when false. Both the queryBuildArgs
// path (QueryText) and the doStart path (SubprocessTransport.Start) are covered.
//
// The doStart case uses a nonexistent CLIPath so cmd.Start() fails after args are
// assembled — cmd.Args is set on exec.Cmd before Start is called, so it is
// accessible even on a failed start.
func TestContinueFlagInArgs(t *testing.T) {
	tests := []struct {
		name                 string
		continueConversation bool
		wantContinue         bool
	}{
		{
			name:                 "ContinueConversation true adds --continue to args",
			continueConversation: true,
			wantContinue:         true,
		},
		{
			name:                 "ContinueConversation false omits --continue from args",
			continueConversation: false,
			wantContinue:         false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			opts := &AgentOptions{ContinueConversation: tt.continueConversation}

			// queryBuildArgs path (QueryText / one-shot API).
			args := queryBuildArgs(opts)
			if got := slices.Contains(args, "--continue"); got != tt.wantContinue {
				t.Errorf("queryBuildArgs: --continue present=%v, want %v (args: %v)", got, tt.wantContinue, args)
			}

			// doStart path (SubprocessTransport).
			tr := &SubprocessTransport{}
			doStartOpts := &AgentOptions{
				ContinueConversation: tt.continueConversation,
				// Nonexistent binary so cmd.Start() fails; cmd.Args is set beforehand.
				CLIPath: "/nonexistent/claude-sdk-test-binary",
			}
			_ = tr.doStart(context.Background(), doStartOpts, "")
			if tr.cmd == nil {
				t.Fatal("doStart: transport.cmd not set before subprocess error")
			}
			if got := slices.Contains(tr.cmd.Args, "--continue"); got != tt.wantContinue {
				t.Errorf("doStart: --continue present=%v, want %v (args: %v)", got, tt.wantContinue, tr.cmd.Args)
			}
		})
	}
}

// TestConflictingResumeFlags verifies that both QueryText and doStart return a
// non-nil error that matches ErrConflictingResumeFlags via errors.Is when
// ContinueConversation=true and ResumeSessionID is non-empty simultaneously.
func TestConflictingResumeFlags(t *testing.T) {
	conflictOpts := &AgentOptions{
		ContinueConversation: true,
		ResumeSessionID:      "abc",
	}

	tests := []struct {
		name    string
		callFn  func() error
	}{
		{
			name: "QueryText returns ErrConflictingResumeFlags",
			callFn: func() error {
				_, err := QueryText(context.Background(), "hello", conflictOpts)
				return err
			},
		},
		{
			name: "doStart returns ErrConflictingResumeFlags",
			callFn: func() error {
				tr := &SubprocessTransport{}
				return tr.doStart(context.Background(), conflictOpts, "abc")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.callFn()
			if err == nil {
				t.Fatal("expected non-nil error, got nil")
			}
			if !errors.Is(err, ErrConflictingResumeFlags) {
				t.Errorf("errors.Is(err, ErrConflictingResumeFlags) = false; got err = %v", err)
			}
		})
	}
}
