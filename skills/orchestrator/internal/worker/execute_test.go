package worker

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// ---------------------------------------------------------------------------
// effectiveCWD: TargetDir takes precedence over WorkerDir when set
// ---------------------------------------------------------------------------

func TestEffectiveCWD(t *testing.T) {
	tests := []struct {
		name      string
		workerDir string
		targetDir string
		wantCWD   string
	}{
		{
			name:      "no TargetDir: use WorkerDir",
			workerDir: "/workspace/workers/phase-1",
			targetDir: "",
			wantCWD:   "/workspace/workers/phase-1",
		},
		{
			name:      "TargetDir set: use TargetDir",
			workerDir: "/workspace/workers/phase-1",
			targetDir: "/Users/joey/skills/orchestrator",
			wantCWD:   "/Users/joey/skills/orchestrator",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cwd := tt.workerDir
			if tt.targetDir != "" {
				cwd = tt.targetDir
			}
			if cwd != tt.wantCWD {
				t.Errorf("effective CWD = %q; want %q", cwd, tt.wantCWD)
			}
		})
	}
}

func TestFormatToolUse_FilePath(t *testing.T) {
	ev := &sdk.StreamedEvent{
		Kind:      sdk.KindToolUse,
		ToolName:  "Read",
		ToolInput: json.RawMessage(`{"file_path":"/Users/joey/main.go"}`),
	}
	got := formatToolUse(ev)
	want := "[tool: Read /Users/joey/main.go]\n"
	if got != want {
		t.Errorf("want %q, got %q", want, got)
	}
}

func TestFormatToolUse_Command(t *testing.T) {
	ev := &sdk.StreamedEvent{
		Kind:      sdk.KindToolUse,
		ToolName:  "Bash",
		ToolInput: json.RawMessage(`{"command":"go test ./..."}`),
	}
	got := formatToolUse(ev)
	want := "[tool: Bash go test ./...]\n"
	if got != want {
		t.Errorf("want %q, got %q", want, got)
	}
}

func TestFormatToolUse_CommandTruncated(t *testing.T) {
	long := strings.Repeat("x", 80)
	ev := &sdk.StreamedEvent{
		Kind:      sdk.KindToolUse,
		ToolName:  "Bash",
		ToolInput: json.RawMessage(`{"command":"` + long + `"}`),
	}
	got := formatToolUse(ev)
	if !strings.HasSuffix(got, "…]\n") {
		t.Errorf("want truncated output ending in ellipsis, got %q", got)
	}
	// 60 chars of command + ellipsis (3 bytes) + "[tool: Bash " + "]\n"
	if len(got) > len("[tool: Bash ")+60+len("…]\n")+5 {
		t.Errorf("output too long: %q", got)
	}
}

func TestFormatToolUse_Pattern(t *testing.T) {
	ev := &sdk.StreamedEvent{
		Kind:      sdk.KindToolUse,
		ToolName:  "Glob",
		ToolInput: json.RawMessage(`{"pattern":"**/*.go"}`),
	}
	got := formatToolUse(ev)
	want := "[tool: Glob **/*.go]\n"
	if got != want {
		t.Errorf("want %q, got %q", want, got)
	}
}

func TestFormatToolUse_NoRecognizedKey(t *testing.T) {
	ev := &sdk.StreamedEvent{
		Kind:      sdk.KindToolUse,
		ToolName:  "TodoWrite",
		ToolInput: json.RawMessage(`{"todos":[]}`),
	}
	got := formatToolUse(ev)
	want := "[tool: TodoWrite]\n"
	if got != want {
		t.Errorf("want %q, got %q", want, got)
	}
}

func TestFormatToolUse_InvalidJSON(t *testing.T) {
	ev := &sdk.StreamedEvent{
		Kind:      sdk.KindToolUse,
		ToolName:  "Read",
		ToolInput: json.RawMessage(`not json`),
	}
	got := formatToolUse(ev)
	want := "[tool: Read]\n"
	if got != want {
		t.Errorf("want %q, got %q", want, got)
	}
}

// ---------------------------------------------------------------------------
// MergeArtifacts error propagation
// ---------------------------------------------------------------------------

func TestMergeArtifacts_HappyPath(t *testing.T) {
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	if err := os.WriteFile(filepath.Join(workerDir, "output.md"), []byte("hello"), 0600); err != nil {
		t.Fatal(err)
	}

	if err := MergeArtifacts(workerDir, phaseDir, mergedDir); err != nil {
		t.Fatalf("expected no error, got: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(phaseDir, "output.md"))
	if err != nil || string(data) != "hello" {
		t.Errorf("phase artifact not copied correctly: err=%v data=%q", err, data)
	}
	data, err = os.ReadFile(filepath.Join(mergedDir, "output.md"))
	if err != nil || string(data) != "hello" {
		t.Errorf("merged artifact not copied correctly: err=%v data=%q", err, data)
	}
}

func TestMergeArtifacts_ReadError(t *testing.T) {
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	// Create a file, then make it unreadable.
	srcFile := filepath.Join(workerDir, "secret.md")
	if err := os.WriteFile(srcFile, []byte("data"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(srcFile, 0000); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(srcFile, 0600) }) // restore so TempDir cleanup can delete

	err := MergeArtifacts(workerDir, phaseDir, mergedDir)
	if err == nil {
		t.Fatal("expected error for unreadable artifact, got nil")
	}
	if !strings.Contains(err.Error(), "reading artifact") {
		t.Errorf("error message should mention 'reading artifact', got: %v", err)
	}
}

func TestMergeArtifacts_WriteError(t *testing.T) {
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	if err := os.WriteFile(filepath.Join(workerDir, "artifact.md"), []byte("content"), 0600); err != nil {
		t.Fatal(err)
	}

	// Make the destination directories read-only so writes fail.
	if err := os.Chmod(phaseDir, 0500); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(mergedDir, 0500); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		os.Chmod(phaseDir, 0700)
		os.Chmod(mergedDir, 0700)
	})

	err := MergeArtifacts(workerDir, phaseDir, mergedDir)
	if err == nil {
		t.Fatal("expected error for unwritable destination, got nil")
	}
}

// ---------------------------------------------------------------------------
// MergeArtifactsWithMeta: frontmatter injection
// ---------------------------------------------------------------------------

func testArtifactMeta() ArtifactMeta {
	ts, _ := time.Parse(time.RFC3339, "2026-03-11T14:00:00Z")
	return ArtifactMeta{
		ProducedBy: "senior-backend-engineer",
		Phase:      "artifact-metadata",
		Workspace:  "ws-test",
		CreatedAt:  ts,
		Confidence: "high",
		DependsOn:  []string{"phase-3"},
	}
}

func TestMergeArtifactsWithMeta_InjectsFrontmatterIntoMarkdown(t *testing.T) {
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	if err := os.WriteFile(filepath.Join(workerDir, "report.md"), []byte("# Report\n\nContent.\n"), 0600); err != nil {
		t.Fatal(err)
	}

	if err := MergeArtifactsWithMeta(workerDir, phaseDir, mergedDir, testArtifactMeta()); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(phaseDir, "report.md"))
	if err != nil {
		t.Fatal(err)
	}

	content := string(data)
	if !strings.HasPrefix(content, "---\n") {
		t.Error("markdown artifact should start with '---\\n' frontmatter")
	}
	if !strings.Contains(content, "produced_by: senior-backend-engineer") {
		t.Error("frontmatter missing produced_by")
	}
	if !strings.Contains(content, "phase: artifact-metadata") {
		t.Error("frontmatter missing phase")
	}
	if !strings.Contains(content, "workspace: ws-test") {
		t.Error("frontmatter missing workspace")
	}
	if !strings.Contains(content, "confidence: high") {
		t.Error("frontmatter missing confidence")
	}
	if !strings.Contains(content, "# Report\n\nContent.") {
		t.Error("original content missing after frontmatter injection")
	}
}

func TestMergeArtifactsWithMeta_SkipsNonMarkdown(t *testing.T) {
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	if err := os.WriteFile(filepath.Join(workerDir, "main.go"), []byte("package main\n"), 0600); err != nil {
		t.Fatal(err)
	}

	if err := MergeArtifactsWithMeta(workerDir, phaseDir, mergedDir, testArtifactMeta()); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(phaseDir, "main.go"))
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "package main\n" {
		t.Errorf("non-markdown file must be copied verbatim; got %q", data)
	}
}

func TestMergeArtifactsWithMeta_DoesNotDoubleInjectFrontmatter(t *testing.T) {
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	existing := "---\nproduced_by: existing\n---\n\n# Content\n"
	if err := os.WriteFile(filepath.Join(workerDir, "artifact.md"), []byte(existing), 0600); err != nil {
		t.Fatal(err)
	}

	if err := MergeArtifactsWithMeta(workerDir, phaseDir, mergedDir, testArtifactMeta()); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(phaseDir, "artifact.md"))
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != existing {
		t.Errorf("file with existing frontmatter must be unchanged;\ngot: %q\nwant: %q", data, existing)
	}
}

func TestMergeArtifactsWithMeta_ZeroMetaSkipsInjection(t *testing.T) {
	// Verify backward-compat: MergeArtifacts (zero meta) does not inject frontmatter.
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	if err := os.WriteFile(filepath.Join(workerDir, "output.md"), []byte("hello"), 0600); err != nil {
		t.Fatal(err)
	}

	if err := MergeArtifacts(workerDir, phaseDir, mergedDir); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(phaseDir, "output.md"))
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "hello" {
		t.Errorf("MergeArtifacts (zero meta) must not inject frontmatter; got %q", data)
	}
}

// ---------------------------------------------------------------------------
// buildWorkerPrompt
// ---------------------------------------------------------------------------

func TestBuildWorkerPrompt_NoPriorContext(t *testing.T) {
	bundle := core.ContextBundle{
		Objective: "Implement the feature",
	}
	got := buildWorkerPrompt(bundle)

	if !strings.Contains(got, "You will be given source material") {
		t.Error("missing preamble in prompt")
	}
	if !strings.Contains(got, "Task: Implement the feature") {
		t.Errorf("missing task in prompt: %q", got)
	}
	if strings.Contains(got, "<prior_phase_output>") {
		t.Error("unexpected XML tags when no prior context provided")
	}
}

func TestBuildWorkerPrompt_WithPriorContext(t *testing.T) {
	bundle := core.ContextBundle{
		Objective:    "Write a summary",
		PriorContext: "The researcher found: X, Y, Z",
	}
	got := buildWorkerPrompt(bundle)

	if !strings.Contains(got, "<prior_phase_output>") {
		t.Error("missing opening <prior_phase_output> XML tag")
	}
	if !strings.Contains(got, "</prior_phase_output>") {
		t.Error("missing closing </prior_phase_output> XML tag")
	}
	if !strings.Contains(got, "The researcher found: X, Y, Z") {
		t.Error("prior context content missing from prompt")
	}
	if !strings.Contains(got, "Task: Write a summary") {
		t.Error("task objective missing from prompt")
	}
	// Task must appear after the prior context block (recency bias).
	priorIdx := strings.Index(got, "<prior_phase_output>")
	taskIdx := strings.Index(got, "Task: Write a summary")
	if taskIdx <= priorIdx {
		t.Error("task must appear after the prior context XML block (recency bias)")
	}
}

func TestBuildWorkerPrompt_TruncationAt8000Chars(t *testing.T) {
	const maxLen = 8000
	longContext := strings.Repeat("a", maxLen+500)
	bundle := core.ContextBundle{
		Objective:    "Process the data",
		PriorContext: longContext,
	}
	got := buildWorkerPrompt(bundle)

	if !strings.Contains(got, "[Note: Prior context truncated") {
		t.Error("expected truncation note when prior context exceeds 8000 chars")
	}
	// Verify that more than maxLen 'a' characters are NOT present in the output.
	if strings.Contains(got, strings.Repeat("a", maxLen+1)) {
		t.Error("prior context was not truncated: more than 8000 chars of content present")
	}
	// Task must still appear.
	if !strings.Contains(got, "Task: Process the data") {
		t.Error("task missing from truncated prompt")
	}
	// Truncation note must include the original length.
	wantLenStr := "8500"
	if !strings.Contains(got, wantLenStr) {
		t.Errorf("truncation note should include original length (%s), got: %q", wantLenStr, got)
	}
}

func TestBuildWorkerPrompt_XMLFramingStructure(t *testing.T) {
	bundle := core.ContextBundle{
		Objective:    "Do the task",
		PriorContext: "Some findings from prior phase",
	}
	got := buildWorkerPrompt(bundle)

	// Opening tag must be followed by a newline.
	if !strings.Contains(got, "<prior_phase_output>\n") {
		t.Error("opening XML tag must be directly followed by newline")
	}
	// Closing tag must be preceded by a newline.
	if !strings.Contains(got, "\n</prior_phase_output>") {
		t.Error("closing XML tag must be preceded by newline")
	}
	// Prior context content must reside inside the XML block.
	openIdx := strings.Index(got, "<prior_phase_output>")
	closeIdx := strings.Index(got, "</prior_phase_output>")
	if openIdx < 0 || closeIdx < 0 || closeIdx <= openIdx {
		t.Fatal("malformed XML framing: open/close tags out of order or missing")
	}
	inner := got[openIdx:closeIdx]
	if !strings.Contains(inner, "Some findings from prior phase") {
		t.Error("prior context not found inside XML block")
	}
}

// ---------------------------------------------------------------------------
// addDirs wiring: TargetDir present → addDirs populated; absent → nil
// ---------------------------------------------------------------------------

// computeEffectiveCWDAndAddDirs replicates the logic in Execute that determines
// the worker's CWD and the --add-dir list passed to sdk.QueryText.
func computeEffectiveCWDAndAddDirs(workerDir, targetDir string) (cwd string, addDirs []string) {
	cwd = workerDir
	if targetDir != "" {
		cwd = targetDir
		addDirs = []string{workerDir}
	}
	return cwd, addDirs
}

func TestAddDirs_RepoTargetedMission(t *testing.T) {
	workerDir := "/workspace/workers/phase-1"
	targetDir := "/Users/joey/my-repo"

	cwd, addDirs := computeEffectiveCWDAndAddDirs(workerDir, targetDir)

	if cwd != targetDir {
		t.Errorf("cwd = %q; want %q", cwd, targetDir)
	}
	if len(addDirs) != 1 || addDirs[0] != workerDir {
		t.Errorf("addDirs = %v; want [%q]", addDirs, workerDir)
	}
}

func TestAddDirs_NonRepoTargetedMission(t *testing.T) {
	workerDir := "/workspace/workers/phase-1"
	targetDir := ""

	cwd, addDirs := computeEffectiveCWDAndAddDirs(workerDir, targetDir)

	if cwd != workerDir {
		t.Errorf("cwd = %q; want %q", cwd, workerDir)
	}
	if len(addDirs) != 0 {
		t.Errorf("addDirs = %v; want empty for non-repo-targeted mission", addDirs)
	}
}

func TestMergeArtifacts_MultipleErrors(t *testing.T) {
	workerDir := t.TempDir()
	phaseDir := t.TempDir()
	mergedDir := t.TempDir()

	// Two unreadable source files → two errors accumulated.
	for _, name := range []string{"a.md", "b.md"} {
		f := filepath.Join(workerDir, name)
		if err := os.WriteFile(f, []byte("x"), 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.Chmod(f, 0000); err != nil {
			t.Fatal(err)
		}
		n := f // capture for closure
		t.Cleanup(func() { os.Chmod(n, 0600) })
	}

	err := MergeArtifacts(workerDir, phaseDir, mergedDir)
	if err == nil {
		t.Fatal("expected joined error, got nil")
	}
	if !strings.Contains(err.Error(), "reading artifact") {
		t.Errorf("joined error should mention 'reading artifact', got: %v", err)
	}
}
