package main

import (
	"strings"
	"testing"
)

func TestRunReviewApprove_RefusesNonAutoIssue(t *testing.T) {
	// Stub tracker returns an issue without the "auto" label.
	writeStubTracker(t, `{"items":[{"id":"abc-001","seq_id":1,"status":"open","labels":"manual"}]}`)

	err := runReviewApprove("TRK-1", "")
	if err == nil {
		t.Fatal("expected error for non-auto issue, got nil")
	}
	if !strings.Contains(err.Error(), "missing 'auto' label") {
		t.Errorf("error message should mention missing auto label, got: %v", err)
	}
}

func TestRunReviewApprove_AcceptsAutoIssue(t *testing.T) {
	// Stub tracker returns an issue already in-progress with the "auto" label —
	// runReviewApprove exits 0 (idempotent) without calling tracker update.
	writeStubTracker(t, `{"items":[{"id":"abc-002","seq_id":2,"status":"in-progress","labels":"auto"}]}`)

	if err := runReviewApprove("TRK-2", ""); err != nil {
		t.Fatalf("unexpected error for auto issue: %v", err)
	}
}
