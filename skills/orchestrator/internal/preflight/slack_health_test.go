package preflight

import (
	"context"
	"testing"
)

func TestSlackHealthSection_NameAndPriority(t *testing.T) {
	s := &slackHealthSection{}
	if got := s.Name(); got != "slack_health" {
		t.Errorf("Name() = %q, want %q", got, "slack_health")
	}
	if got := s.Priority(); got != 18 {
		t.Errorf("Priority() = %d, want 18", got)
	}
}

func TestSlackHealthSection_BinaryNotFound(t *testing.T) {
	// Set a PATH that doesn't contain slack-plugin-fsck
	t.Setenv("PATH", "/usr/bin:/bin")

	s := &slackHealthSection{}
	blk, err := s.Fetch(context.Background())

	// Should return empty block (silently omit), no error
	if err != nil {
		t.Errorf("expected no error, got %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for missing binary, got %q", blk.Body)
	}
}

func TestSlackHealthSection_RegisteredInInit(t *testing.T) {
	// Verify the init() registered a section named "slack_health".
	found := false
	for _, s := range List() {
		if s.Name() == "slack_health" {
			found = true
			break
		}
	}
	if !found {
		t.Error("slackHealthSection not found in registry after init()")
	}
}

func TestSlackHealthSection_FetchWithCommand(t *testing.T) {
	s := &slackHealthSection{}
	blk, err := s.Fetch(context.Background())

	// Should return a block (either with warning or empty).
	if err != nil {
		t.Errorf("unexpected error: %v", err)
	}
	if blk.Title != "Slack plugin health" {
		t.Errorf("expected title 'Slack plugin health', got %q", blk.Title)
	}
	// The body may be empty (if exit 0) or contain a warning (if exit non-zero).
	// We just verify the section works without error.
	t.Logf("Block title: %s, body: %q", blk.Title, blk.Body)
}
