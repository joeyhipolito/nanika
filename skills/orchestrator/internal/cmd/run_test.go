package cmd

import (
	"bytes"
	"errors"
	"io"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/routing"
)

// ---------------------------------------------------------------------------
// missionSucceeded
// ---------------------------------------------------------------------------

func TestMissionSucceeded_TrueWhenErrNilAndSuccess(t *testing.T) {
	result := &core.ExecutionResult{Success: true}
	if !missionSucceeded(result, nil) {
		t.Error("missionSucceeded should return true when err=nil and result.Success=true")
	}
}

func TestMissionSucceeded_FalseWhenErr(t *testing.T) {
	result := &core.ExecutionResult{Success: true}
	if missionSucceeded(result, errors.New("exec error")) {
		t.Error("missionSucceeded should return false when err != nil even if result.Success=true")
	}
}

func TestMissionSucceeded_FalseWhenResultNil(t *testing.T) {
	if missionSucceeded(nil, nil) {
		t.Error("missionSucceeded should return false when result is nil")
	}
}

func TestMissionSucceeded_FalseWhenSuccessFalse(t *testing.T) {
	result := &core.ExecutionResult{Success: false}
	if missionSucceeded(result, nil) {
		t.Error("missionSucceeded should return false when result.Success=false")
	}
}

func TestMergeDecompInsights_AuditedWinsOverPassive(t *testing.T) {
	repeated := []routing.DecompFinding{
		{
			FindingType: "missing_phase",
			Detail:      "add testing phase",
			WorkspaceID: "ws-a",
			AuditScore:  4,
			CreatedAt:   time.Now(),
		},
		{
			FindingType: "missing_phase",
			Detail:      "add testing phase",
			WorkspaceID: "ws-b",
			AuditScore:  4,
			CreatedAt:   time.Now(),
		},
	}
	passive := []routing.DecompFinding{
		{
			FindingType:  "missing_phase",
			Detail:       "add testing phase",
			WorkspaceID:  "ws-c",
			AuditScore:   0,
			DecompSource: "passive",
			CreatedAt:    time.Now(),
		},
	}

	insights := mergeDecompInsights(repeated, passive)
	if len(insights) != 1 {
		t.Fatalf("len(insights) = %d, want 1", len(insights))
	}
	if insights[0].Count != 2 {
		t.Errorf("Count = %d, want 2 (audited count only)", insights[0].Count)
	}
}

func TestMergeDecompInsights_PassiveFillsGap(t *testing.T) {
	passive := []routing.DecompFinding{
		{
			FindingType:  "low_confidence",
			Detail:       "all phases assigned by keyword",
			WorkspaceID:  "ws-a",
			AuditScore:   0,
			DecompSource: "passive",
			CreatedAt:    time.Now(),
		},
		{
			FindingType:  "low_confidence",
			Detail:       "all phases assigned by keyword",
			WorkspaceID:  "ws-b",
			AuditScore:   0,
			DecompSource: "passive",
			CreatedAt:    time.Now(),
		},
	}

	insights := mergeDecompInsights(nil, passive)
	if len(insights) != 1 {
		t.Fatalf("len(insights) = %d, want 1", len(insights))
	}
	if insights[0].FindingType != "low_confidence" {
		t.Errorf("FindingType = %q, want low_confidence", insights[0].FindingType)
	}
	if insights[0].Count != 2 {
		t.Errorf("Count = %d, want 2", insights[0].Count)
	}
}

func TestPrintEmitterDropWarning_PrintsWhenDropsPresent(t *testing.T) {
	oldStderr := os.Stderr
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	os.Stderr = w
	defer func() {
		os.Stderr = oldStderr
	}()

	printEmitterDropWarning(dropReporterStub{stats: event.DropStats{
		FileDroppedWrites: 2,
		UDSDroppedWrites:  1,
	}})

	_ = w.Close()
	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("read stderr: %v", err)
	}
	got := buf.String()
	if !strings.Contains(got, "warning: event delivery drops observed") {
		t.Fatalf("expected warning prefix, got %q", got)
	}
	if !strings.Contains(got, "file=2") || !strings.Contains(got, "uds=1") {
		t.Fatalf("expected file/uds counts in warning, got %q", got)
	}
}

func TestPrintEmitterDropWarning_SilentWhenNoDrops(t *testing.T) {
	oldStderr := os.Stderr
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	os.Stderr = w
	defer func() {
		os.Stderr = oldStderr
	}()

	printEmitterDropWarning(dropReporterStub{})

	_ = w.Close()
	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("read stderr: %v", err)
	}
	if got := buf.String(); got != "" {
		t.Fatalf("expected no warning, got %q", got)
	}
}

func TestWarnMissingWorkspaceLinks_WarnsForMissingIssueAndMissionPath(t *testing.T) {
	oldStderr := os.Stderr
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	os.Stderr = w
	defer func() {
		os.Stderr = oldStderr
	}()

	warnMissingWorkspaceLinks(missionFrontmatter{}, "")

	_ = w.Close()
	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("read stderr: %v", err)
	}
	got := buf.String()
	if !strings.Contains(got, "no linear_issue_id in mission frontmatter") {
		t.Fatalf("expected missing issue warning, got %q", got)
	}
	if !strings.Contains(got, "no mission file path") {
		t.Fatalf("expected missing mission path warning, got %q", got)
	}
}

func TestWarnMissingWorkspaceLinks_WarnsOnlyForMissingIssue(t *testing.T) {
	oldStderr := os.Stderr
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	os.Stderr = w
	defer func() {
		os.Stderr = oldStderr
	}()

	warnMissingWorkspaceLinks(missionFrontmatter{}, "/tmp/mission.md")

	_ = w.Close()
	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("read stderr: %v", err)
	}
	got := buf.String()
	if !strings.Contains(got, "no linear_issue_id in mission frontmatter") {
		t.Fatalf("expected missing issue warning, got %q", got)
	}
	if strings.Contains(got, "no mission file path") {
		t.Fatalf("did not expect missing mission-path warning, got %q", got)
	}
}

func TestWarnMissingWorkspaceLinks_SilentWhenFullyLinked(t *testing.T) {
	oldStderr := os.Stderr
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	os.Stderr = w
	defer func() {
		os.Stderr = oldStderr
	}()

	warnMissingWorkspaceLinks(missionFrontmatter{LinearIssueID: "V-5"}, "/tmp/mission.md")

	_ = w.Close()
	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("read stderr: %v", err)
	}
	if got := buf.String(); got != "" {
		t.Fatalf("expected no warning, got %q", got)
	}
}

func TestNormalizeExecutionResult_NilWithError(t *testing.T) {
	plan := &core.Plan{ID: "plan-1"}

	got := normalizeExecutionResult(plan, nil, errors.New("boom"))

	if got == nil {
		t.Fatal("normalizeExecutionResult returned nil")
	}
	if got.Success {
		t.Fatal("normalizeExecutionResult should mark nil result as failure")
	}
	if got.Error != "boom" {
		t.Fatalf("Error = %q, want boom", got.Error)
	}
	if got.Plan != plan {
		t.Fatal("normalizeExecutionResult should preserve the plan on synthesized results")
	}
}

func TestNormalizeExecutionResult_PopulatesMissingPlanAndError(t *testing.T) {
	plan := &core.Plan{ID: "plan-2"}
	result := &core.ExecutionResult{}

	got := normalizeExecutionResult(plan, result, errors.New("phase failed"))

	if got.Plan != plan {
		t.Fatal("normalizeExecutionResult should populate missing plan")
	}
	if got.Error != "phase failed" {
		t.Fatalf("Error = %q, want phase failed", got.Error)
	}
}

type dropReporterStub struct {
	event.NoOpEmitter
	stats event.DropStats
}

func (d dropReporterStub) DropStats() event.DropStats { return d.stats }
