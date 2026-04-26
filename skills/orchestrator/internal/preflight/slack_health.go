package preflight

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

func init() {
	Register(&slackHealthSection{})
}

// slackHealthSection is a preflight.Section that checks the health of the
// slack-plugin by running `slack-plugin-fsck --check` with a 2-second timeout.
type slackHealthSection struct{}

func (s *slackHealthSection) Name() string  { return "slack_health" }
func (s *slackHealthSection) Priority() int { return 18 }

// Fetch executes slack-plugin-fsck --check with a 2-second timeout.
// If the binary is not found, returns an empty block (section is silently omitted).
// If the command exits 0, returns an empty block.
// If the command exits non-zero, returns a warning block with the first stdout line.
func (s *slackHealthSection) Fetch(ctx context.Context) (Block, error) {
	// Create a timeout context with 2 seconds.
	timeoutCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()

	cmd := exec.CommandContext(timeoutCtx, "slack-plugin-fsck", "--check")
	output, err := cmd.CombinedOutput()

	// Check if the binary was not found (ENOENT / executable file not found).
	// This check happens before we look at exit codes.
	if err != nil && os.IsNotExist(err) {
		// Binary not on PATH — return empty block to omit section.
		return Block{Title: "Slack plugin health"}, nil
	}
	if err != nil && strings.Contains(err.Error(), "executable file not found") {
		// Binary not on PATH — return empty block to omit section.
		return Block{Title: "Slack plugin health"}, nil
	}

	// Check for context timeout.
	if timeoutCtx.Err() == context.DeadlineExceeded {
		// Timeout is non-fatal — return empty block.
		return Block{Title: "Slack plugin health"}, nil
	}

	// Get the exit code. ProcessState should be set even if err is non-nil (for ExitError).
	if cmd.ProcessState == nil {
		// Command didn't run — return empty block.
		return Block{Title: "Slack plugin health"}, nil
	}

	exitCode := cmd.ProcessState.ExitCode()

	// If exit code is 0, return empty block.
	if exitCode == 0 {
		return Block{Title: "Slack plugin health"}, nil
	}

	// Exit code is non-zero, format the warning block.
	outputStr := strings.TrimSpace(string(output))
	lines := strings.Split(outputStr, "\n")
	var warningLine string
	if len(lines) > 0 {
		warningLine = lines[0]
	}

	body := fmt.Sprintf("WARNING: %s — run `slack-plugin-fsck --yes` to clean up\n", warningLine)

	return Block{
		Title: "Slack plugin health",
		Body:  body,
	}, nil
}
