// Package post executes platform CLIs to publish approved comment drafts.
package post

import (
	"context"
	"fmt"
	"os/exec"
	"strings"

	"github.com/joeyhipolito/nanika-engage/internal/queue"
)

// PlatformCLIs maps each supported platform to its CLI binary name.
var PlatformCLIs = map[string]string{
	"linkedin": "linkedin",
	"youtube":  "youtube",
	"reddit":   "reddit",
	"substack": "substack",
}

// CheckCLI returns an error if the named binary is not found in PATH.
func CheckCLI(name string) error {
	if _, err := exec.LookPath(name); err != nil {
		return fmt.Errorf("%s not found in PATH", name)
	}
	return nil
}

// Post dispatches d to the correct platform CLI and returns the CLI stdout.
// The caller is responsible for transitioning the draft state after success.
func Post(ctx context.Context, d *queue.Draft) (string, error) {
	if d.State != queue.StateApproved {
		return "", fmt.Errorf("draft %s is %s, not approved", d.ID, d.State)
	}
	switch d.Platform {
	case "linkedin":
		return postLinkedIn(ctx, d)
	case "youtube":
		return postYouTube(ctx, d)
	case "reddit":
		return postReddit(ctx, d)
	case "substack":
		return postSubstack(ctx, d)
	default:
		return "", fmt.Errorf("unsupported platform: %s", d.Platform)
	}
}

// postLinkedIn runs: linkedin comment <activity-urn> <text>
func postLinkedIn(ctx context.Context, d *queue.Draft) (string, error) {
	out, err := runCLI(ctx, "linkedin", "comment", d.Opportunity.ID, d.Comment)
	if err != nil {
		return "", fmt.Errorf("linkedin comment: %w", err)
	}
	return string(out), nil
}

// postYouTube runs: youtube comment <video-id> <text>
func postYouTube(ctx context.Context, d *queue.Draft) (string, error) {
	out, err := runCLI(ctx, "youtube", "comment", d.Opportunity.ID, d.Comment)
	if err != nil {
		return "", fmt.Errorf("youtube comment: %w", err)
	}
	return string(out), nil
}

// postReddit runs: reddit comment <post-id> <text>
func postReddit(ctx context.Context, d *queue.Draft) (string, error) {
	out, err := runCLI(ctx, "reddit", "comment", d.Opportunity.ID, d.Comment)
	if err != nil {
		return "", fmt.Errorf("reddit comment: %w", err)
	}
	return string(out), nil
}

// postSubstack runs: substack comment <post-url> <text>
// Substack identifies posts by URL, not a short ID.
func postSubstack(ctx context.Context, d *queue.Draft) (string, error) {
	target := d.Opportunity.URL
	if target == "" {
		target = d.Opportunity.ID
	}
	out, err := runCLI(ctx, "substack", "comment", target, d.Comment)
	if err != nil {
		return "", fmt.Errorf("substack comment: %w", err)
	}
	return string(out), nil
}

func runCLI(ctx context.Context, name string, args ...string) ([]byte, error) {
	out, err := exec.CommandContext(ctx, name, args...).Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			return nil, fmt.Errorf("%s: %s", name, strings.TrimSpace(string(ee.Stderr)))
		}
		return nil, fmt.Errorf("running %s %s: %w", name, strings.Join(args, " "), err)
	}
	return out, nil
}
