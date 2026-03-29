package routing

import (
	"os"
	"path/filepath"
	"testing"
)

// writeFile creates a file at path with the given content, failing the test on error.
func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("writeFile %s: %v", path, err)
	}
}

// TestDetectProfile_Go verifies Go repo detection with cobra framework.
func TestDetectProfile_Go(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "go.mod"), `module github.com/example/myapp

go 1.21

require github.com/spf13/cobra v1.8.0
`)
	writeFile(t, filepath.Join(dir, "Makefile"), "build:\n\tgo build ./...\ntest:\n\tgo test ./...\n")

	p := DetectProfile(dir)

	if p.Language != "go" {
		t.Errorf("Language = %q, want %q", p.Language, "go")
	}
	if p.Runtime != "go" {
		t.Errorf("Runtime = %q, want %q", p.Runtime, "go")
	}
	if p.TestCommand != "go test ./..." {
		t.Errorf("TestCommand = %q, want %q", p.TestCommand, "go test ./...")
	}
	// Makefile has a build target — should prefer "make build" over generic fallback.
	if p.BuildCommand != "make build" {
		t.Errorf("BuildCommand = %q, want %q", p.BuildCommand, "make build")
	}
	if p.Framework != "cobra" {
		t.Errorf("Framework = %q, want %q", p.Framework, "cobra")
	}
}

// TestDetectProfile_GoNoMakefile verifies fallback to "go build ./..." when no Makefile.
func TestDetectProfile_GoNoMakefile(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "go.mod"), "module github.com/example/myapp\n\ngo 1.21\n")

	p := DetectProfile(dir)

	if p.BuildCommand != "go build ./..." {
		t.Errorf("BuildCommand = %q, want %q", p.BuildCommand, "go build ./...")
	}
}

// TestDetectProfile_Rust verifies Rust repo detection with axum framework.
func TestDetectProfile_Rust(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "Cargo.toml"), `[package]
name = "myservice"
version = "0.1.0"

[dependencies]
axum = "0.7"
tokio = { version = "1", features = ["full"] }
`)

	p := DetectProfile(dir)

	if p.Language != "rust" {
		t.Errorf("Language = %q, want %q", p.Language, "rust")
	}
	if p.Runtime != "cargo" {
		t.Errorf("Runtime = %q, want %q", p.Runtime, "cargo")
	}
	if p.TestCommand != "cargo test" {
		t.Errorf("TestCommand = %q, want %q", p.TestCommand, "cargo test")
	}
	if p.BuildCommand != "cargo build --release" {
		t.Errorf("BuildCommand = %q, want %q", p.BuildCommand, "cargo build --release")
	}
	if p.Framework != "axum" {
		t.Errorf("Framework = %q, want %q", p.Framework, "axum")
	}
}

// TestDetectProfile_NodeReact verifies Node/React detection.
func TestDetectProfile_NodeReact(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "package.json"), `{
  "name": "my-app",
  "scripts": { "test": "jest", "build": "tsc" },
  "dependencies": { "react": "^18.0.0" }
}`)
	writeFile(t, filepath.Join(dir, "tsconfig.json"), "{}")

	p := DetectProfile(dir)

	if p.Language != "typescript" {
		t.Errorf("Language = %q, want %q", p.Language, "typescript")
	}
	if p.Runtime != "node" {
		t.Errorf("Runtime = %q, want %q", p.Runtime, "node")
	}
	if p.Framework != "react" {
		t.Errorf("Framework = %q, want %q", p.Framework, "react")
	}
	if p.TestCommand != "npm test" {
		t.Errorf("TestCommand = %q, want %q", p.TestCommand, "npm test")
	}
}

// TestDetectProfile_NodeJavaScript verifies that no tsconfig.json means javascript.
func TestDetectProfile_NodeJavaScript(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "package.json"), `{"name":"app","dependencies":{"express":"^4"}}`)
	// no tsconfig.json

	p := DetectProfile(dir)

	if p.Language != "javascript" {
		t.Errorf("Language = %q, want %q", p.Language, "javascript")
	}
	if p.Framework != "express" {
		t.Errorf("Framework = %q, want %q", p.Framework, "express")
	}
}

// TestDetectProfile_Python verifies Python/FastAPI detection.
func TestDetectProfile_Python(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "requirements.txt"), "fastapi==0.100.0\nuvicorn==0.23.0\n")

	p := DetectProfile(dir)

	if p.Language != "python" {
		t.Errorf("Language = %q, want %q", p.Language, "python")
	}
	if p.Runtime != "python" {
		t.Errorf("Runtime = %q, want %q", p.Runtime, "python")
	}
	if p.Framework != "fastapi" {
		t.Errorf("Framework = %q, want %q", p.Framework, "fastapi")
	}
}

// TestDetectProfile_KeyDirectories verifies that notable dirs are captured.
func TestDetectProfile_KeyDirectories(t *testing.T) {
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "go.mod"), "module m\ngo 1.21\n")
	for _, d := range []string{"cmd", "internal", "docs", ".git", "vendor"} {
		if err := os.Mkdir(filepath.Join(dir, d), 0o755); err != nil {
			t.Fatalf("mkdir %s: %v", d, err)
		}
	}

	p := DetectProfile(dir)

	dirSet := make(map[string]bool, len(p.KeyDirectories))
	for _, d := range p.KeyDirectories {
		dirSet[d] = true
	}
	for _, want := range []string{"cmd", "internal", "docs"} {
		if !dirSet[want] {
			t.Errorf("KeyDirectories missing %q, got %v", want, p.KeyDirectories)
		}
	}
	// Hidden dirs and non-notable dirs should not appear.
	for _, notWant := range []string{".git", "vendor"} {
		if dirSet[notWant] {
			t.Errorf("KeyDirectories should not contain %q", notWant)
		}
	}
}

// TestDetectProfile_Empty verifies that an empty directory returns an empty profile without error.
func TestDetectProfile_Empty(t *testing.T) {
	dir := t.TempDir()
	p := DetectProfile(dir)
	if p.Language != "" || p.Runtime != "" || p.TestCommand != "" {
		t.Errorf("empty dir should yield empty profile, got language=%q runtime=%q testCmd=%q",
			p.Language, p.Runtime, p.TestCommand)
	}
}

// TestDetectProfile_Inaccessible verifies graceful fallback for non-existent paths.
func TestDetectProfile_Inaccessible(t *testing.T) {
	p := DetectProfile("/nonexistent/path/that/does/not/exist")
	if p.Language != "" || p.Runtime != "" {
		t.Errorf("inaccessible path should yield empty profile, got %+v", p)
	}
}

// TestDetectProfile_MakefileOverridesEmpty verifies Makefile test target fills empty TestCommand.
func TestDetectProfile_MakefileOverridesEmpty(t *testing.T) {
	// No recognized language file — only a Makefile with test/build targets.
	dir := t.TempDir()
	writeFile(t, filepath.Join(dir, "Makefile"), "build:\n\t./build.sh\ntest:\n\t./test.sh\n")

	p := DetectProfile(dir)

	// Language undetected but make targets should NOT fill in commands because
	// DetectProfile only applies Makefile overrides when the commands are still "".
	// For unknown language, Language/Runtime stay empty.
	if p.Language != "" {
		t.Errorf("unexpected Language %q for Makefile-only repo", p.Language)
	}
	// Makefile targets for test/build only fill in when the primary language block
	// sets TestCommand/BuildCommand to "". For an unknown language block, those
	// fields start as "" so the Makefile override should set them.
	if p.TestCommand != "make test" {
		t.Errorf("TestCommand = %q, want %q", p.TestCommand, "make test")
	}
	if p.BuildCommand != "make build" {
		t.Errorf("BuildCommand = %q, want %q", p.BuildCommand, "make build")
	}
}

// ─── MergeDetected ────────────────────────────────────────────────────────────

// TestMergeDetected_DBWins verifies that non-empty DB fields are never overwritten.
func TestMergeDetected_DBWins(t *testing.T) {
	db := TargetProfile{
		Language:    "go",
		Runtime:     "go",
		TestCommand: "make test",
		BuildCommand: "make build",
		Framework:   "cobra",
		KeyDirectories: []string{"cmd", "internal"},
	}
	detected := TargetProfile{
		Language:    "rust",
		Runtime:     "cargo",
		TestCommand: "cargo test",
		BuildCommand: "cargo build --release",
		Framework:   "axum",
		KeyDirectories: []string{"src"},
	}

	merged := MergeDetected(db, detected)

	if merged.Language != "go" {
		t.Errorf("Language = %q, want DB value %q", merged.Language, "go")
	}
	if merged.Runtime != "go" {
		t.Errorf("Runtime = %q, want DB value %q", merged.Runtime, "go")
	}
	if merged.TestCommand != "make test" {
		t.Errorf("TestCommand = %q, want DB value %q", merged.TestCommand, "make test")
	}
	if merged.BuildCommand != "make build" {
		t.Errorf("BuildCommand = %q, want DB value %q", merged.BuildCommand, "make build")
	}
	if merged.Framework != "cobra" {
		t.Errorf("Framework = %q, want DB value %q", merged.Framework, "cobra")
	}
	if len(merged.KeyDirectories) != 2 || merged.KeyDirectories[0] != "cmd" {
		t.Errorf("KeyDirectories = %v, want DB value [cmd internal]", merged.KeyDirectories)
	}
}

// TestMergeDetected_FillsZeros verifies that detected values fill zero-value DB fields.
func TestMergeDetected_FillsZeros(t *testing.T) {
	db := TargetProfile{} // all zero
	detected := TargetProfile{
		Language:       "go",
		Runtime:        "go",
		TestCommand:    "go test ./...",
		BuildCommand:   "go build ./...",
		Framework:      "gin",
		KeyDirectories: []string{"cmd"},
	}

	merged := MergeDetected(db, detected)

	if merged.Language != "go" {
		t.Errorf("Language = %q, want %q", merged.Language, "go")
	}
	if merged.TestCommand != "go test ./..." {
		t.Errorf("TestCommand = %q, want %q", merged.TestCommand, "go test ./...")
	}
	if merged.Framework != "gin" {
		t.Errorf("Framework = %q, want %q", merged.Framework, "gin")
	}
	if len(merged.KeyDirectories) == 0 {
		t.Error("KeyDirectories should be filled from detected, got empty")
	}
}

// TestMergeDetected_PartialDB verifies that only zero-value DB fields are filled.
func TestMergeDetected_PartialDB(t *testing.T) {
	db := TargetProfile{
		Language: "go",
		// Runtime, TestCommand, BuildCommand, Framework, KeyDirectories all zero
	}
	detected := TargetProfile{
		Language:     "rust",   // should NOT overwrite db.Language
		Runtime:      "go",
		TestCommand:  "go test ./...",
		BuildCommand: "make build",
		Framework:    "cobra",
	}

	merged := MergeDetected(db, detected)

	if merged.Language != "go" {
		t.Errorf("Language = %q, want DB %q", merged.Language, "go")
	}
	if merged.Runtime != "go" {
		t.Errorf("Runtime = %q, want detected %q", merged.Runtime, "go")
	}
	if merged.TestCommand != "go test ./..." {
		t.Errorf("TestCommand = %q, want detected %q", merged.TestCommand, "go test ./...")
	}
}
