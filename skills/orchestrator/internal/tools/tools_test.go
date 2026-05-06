package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

// ─── Schema validation ────────────────────────────────────────────────────────

// schemaValid checks that schema is a valid JSON Schema draft-7 object descriptor.
func schemaValid(t *testing.T, name string, schema json.RawMessage) {
	t.Helper()
	var doc struct {
		Schema     string          `json:"$schema"`
		Type       string          `json:"type"`
		Properties map[string]any  `json:"properties"`
		Required   []string        `json:"required"`
	}
	if err := json.Unmarshal(schema, &doc); err != nil {
		t.Errorf("%s: schema is not valid JSON: %v", name, err)
		return
	}
	if doc.Schema != draft7URI {
		t.Errorf("%s: $schema = %q, want %q", name, doc.Schema, draft7URI)
	}
	if doc.Type != "object" {
		t.Errorf("%s: type = %q, want %q", name, doc.Type, "object")
	}
	// Every required field must appear in properties.
	for _, req := range doc.Required {
		if _, ok := doc.Properties[req]; !ok {
			t.Errorf("%s: required field %q not in properties", name, req)
		}
	}
}

func TestAllToolSchemasValid(t *testing.T) {
	tools := tier1Tools()
	for _, tool := range tools {
		schemaValid(t, tool.Name(), tool.InputSchema())
	}
}

// ─── Bash tool ────────────────────────────────────────────────────────────────

func TestBashTool_Execute_Simple(t *testing.T) {
	bash := NewBashTool()
	ctx := context.Background()
	res, err := bash.Execute(ctx, map[string]any{"command": "echo hello"})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatalf("unexpected error: %s", res.Content)
	}
	if !strings.Contains(res.Content, "hello") {
		t.Errorf("expected 'hello' in output, got: %q", res.Content)
	}
}

func TestBashTool_Execute_ExitCode(t *testing.T) {
	bash := NewBashTool()
	res, err := bash.Execute(context.Background(), map[string]any{"command": "exit 1"})
	if err != nil {
		t.Fatal(err)
	}
	if !res.IsError {
		t.Error("expected IsError=true for non-zero exit")
	}
}

func TestBashTool_Execute_Timeout(t *testing.T) {
	bash := &BashTool{defaultTimeout: defaultBashTimeout}
	ctx := context.Background()
	start := time.Now()
	res, err := bash.Execute(ctx, map[string]any{
		"command":         "sleep 10",
		"timeout_seconds": float64(1),
	})
	elapsed := time.Since(start)
	if err != nil {
		t.Fatal(err)
	}
	if elapsed > 3*time.Second {
		t.Errorf("timeout not respected: elapsed=%v", elapsed)
	}
	if !res.IsError {
		t.Error("expected IsError=true after timeout")
	}
}

func TestBashTool_Execute_MissingCommand(t *testing.T) {
	bash := NewBashTool()
	res, err := bash.Execute(context.Background(), map[string]any{})
	if err != nil {
		t.Fatal(err)
	}
	if !res.IsError {
		t.Error("expected IsError=true for missing command")
	}
}

func TestBashTool_Stream(t *testing.T) {
	bash := NewBashTool().(Streamer)
	var sb strings.Builder
	err := bash.Stream(context.Background(), map[string]any{"command": "printf 'a\nb\nc'"}, &sb)
	if err != nil {
		t.Fatalf("stream error: %v", err)
	}
	if !strings.Contains(sb.String(), "a") {
		t.Errorf("expected streamed output, got %q", sb.String())
	}
}

func TestBashTool_Execute_WorkingDir(t *testing.T) {
	tmp := t.TempDir()
	bash := NewBashTool()
	res, err := bash.Execute(context.Background(), map[string]any{
		"command":     "pwd",
		"working_dir": tmp,
	})
	if err != nil {
		t.Fatal(err)
	}
	// pwd on macOS may add /private prefix; compare with EvalSymlinks.
	got, _ := filepath.EvalSymlinks(strings.TrimSpace(res.Content))
	want, _ := filepath.EvalSymlinks(tmp)
	if got != want {
		t.Errorf("working_dir: got %q, want %q", got, want)
	}
}

// ─── FileRead tool ────────────────────────────────────────────────────────────

func TestFileReadTool_Execute_All(t *testing.T) {
	tmp := writeTemp(t, "line1\nline2\nline3\n")
	tool := NewFileReadTool()
	res, err := tool.Execute(context.Background(), map[string]any{"path": tmp})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	if !strings.Contains(res.Content, "line2") {
		t.Errorf("expected full content, got %q", res.Content)
	}
}

func TestFileReadTool_Execute_OffsetLimit(t *testing.T) {
	tmp := writeTemp(t, "L1\nL2\nL3\nL4\nL5\n")
	tool := NewFileReadTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"path":   tmp,
		"offset": float64(2),
		"limit":  float64(2),
	})
	if err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimRight(res.Content, "\n"), "\n")
	if len(lines) != 2 || lines[0] != "L2" || lines[1] != "L3" {
		t.Errorf("offset/limit: got %q", res.Content)
	}
}

func TestFileReadTool_Execute_MissingFile(t *testing.T) {
	tool := NewFileReadTool()
	res, err := tool.Execute(context.Background(), map[string]any{"path": "/nonexistent/file.txt"})
	if err != nil {
		t.Fatal(err)
	}
	if !res.IsError {
		t.Error("expected IsError for missing file")
	}
}

// ─── FileWrite tool ───────────────────────────────────────────────────────────

func TestFileWriteTool_Execute_Atomic(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "out.txt")
	tool := NewFileWriteTool()
	content := "atomic content"
	res, err := tool.Execute(context.Background(), map[string]any{
		"path":    path,
		"content": content,
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != content {
		t.Errorf("content mismatch: got %q want %q", got, content)
	}
}

func TestFileWriteTool_Execute_CreatesParentDirs(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "a", "b", "c.txt")
	tool := NewFileWriteTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"path":    path,
		"content": "hello",
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	if _, err := os.Stat(path); err != nil {
		t.Errorf("file not created: %v", err)
	}
}

// ─── FileEdit tool ────────────────────────────────────────────────────────────

func TestFileEditTool_Execute_Exact(t *testing.T) {
	path := writeTemp(t, "hello world\n")
	tool := NewFileEditTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"path":       path,
		"old_string": "world",
		"new_string": "Go",
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	got, _ := os.ReadFile(path)
	if !strings.Contains(string(got), "Go") {
		t.Errorf("edit not applied: %q", got)
	}
}

func TestFileEditTool_Execute_FuzzyMatch(t *testing.T) {
	// Old string has different indentation than the file.
	content := "func foo() {\n\treturn 42\n}\n"
	path := writeTemp(t, content)
	tool := NewFileEditTool()
	// old_string uses spaces instead of tabs — fuzzy match should find it.
	res, err := tool.Execute(context.Background(), map[string]any{
		"path":       path,
		"old_string": "func foo() {\n    return 42\n}\n",
		"new_string": "func foo() {\n\treturn 99\n}\n",
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	got, _ := os.ReadFile(path)
	if !strings.Contains(string(got), "99") {
		t.Errorf("fuzzy edit not applied: %q", got)
	}
}

func TestFileEditTool_Execute_Ambiguous(t *testing.T) {
	path := writeTemp(t, "x\nx\nx\n")
	tool := NewFileEditTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"path":       path,
		"old_string": "x",
		"new_string": "y",
	})
	if err != nil {
		t.Fatal(err)
	}
	if !res.IsError {
		t.Error("expected error for ambiguous match")
	}
}

func TestFileEditTool_Execute_NotFound(t *testing.T) {
	path := writeTemp(t, "hello\n")
	tool := NewFileEditTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"path":       path,
		"old_string": "notpresent",
		"new_string": "x",
	})
	if err != nil {
		t.Fatal(err)
	}
	if !res.IsError {
		t.Error("expected error for not-found string")
	}
}

// ─── Glob tool ────────────────────────────────────────────────────────────────

func TestGlobTool_Execute_Simple(t *testing.T) {
	dir := t.TempDir()
	touch(t, filepath.Join(dir, "a.go"))
	touch(t, filepath.Join(dir, "b.go"))
	touch(t, filepath.Join(dir, "c.txt"))

	tool := NewGlobTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"pattern": "*.go",
		"dir":     dir,
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	if !strings.Contains(res.Content, "a.go") || !strings.Contains(res.Content, "b.go") {
		t.Errorf("expected .go files in output: %q", res.Content)
	}
	if strings.Contains(res.Content, "c.txt") {
		t.Errorf("unexpected .txt in output: %q", res.Content)
	}
}

func TestGlobTool_Execute_Doublestar(t *testing.T) {
	dir := t.TempDir()
	sub := filepath.Join(dir, "sub")
	os.Mkdir(sub, 0o755)
	touch(t, filepath.Join(sub, "deep.go"))

	tool := NewGlobTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"pattern": "**/*.go",
		"dir":     dir,
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	if !strings.Contains(res.Content, "deep.go") {
		t.Errorf("expected deep.go from ** glob: %q", res.Content)
	}
}

func TestGlobTool_Execute_NoMatches(t *testing.T) {
	dir := t.TempDir()
	tool := NewGlobTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"pattern": "*.nonexistent",
		"dir":     dir,
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	if !strings.Contains(res.Content, "no matches") {
		t.Errorf("expected 'no matches', got %q", res.Content)
	}
}

// ─── Grep tool ────────────────────────────────────────────────────────────────

func TestGrepTool_Execute_Simple(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "f.txt")
	os.WriteFile(path, []byte("apple\nbanana\napricot\n"), 0o644)

	tool := NewGrepTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"pattern": "ap",
		"path":    path,
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	if !strings.Contains(res.Content, "apple") {
		t.Errorf("expected 'apple' in results: %q", res.Content)
	}
	if strings.Contains(res.Content, "banana") {
		t.Errorf("unexpected 'banana' in results: %q", res.Content)
	}
}

func TestGrepTool_Execute_CaseInsensitive(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "f.txt")
	os.WriteFile(path, []byte("Hello World\nhello world\n"), 0o644)

	tool := NewGrepTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"pattern":          "HELLO",
		"path":             path,
		"case_insensitive": true,
	})
	if err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimRight(res.Content, "\n"), "\n")
	if len(lines) != 2 {
		t.Errorf("expected 2 case-insensitive matches, got %d: %q", len(lines), res.Content)
	}
}

func TestGrepTool_Execute_NoMatches(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "f.txt")
	os.WriteFile(path, []byte("no match here\n"), 0o644)

	tool := NewGrepTool()
	res, err := tool.Execute(context.Background(), map[string]any{
		"pattern": "zzznope",
		"path":    path,
	})
	if err != nil {
		t.Fatal(err)
	}
	if res.IsError {
		t.Fatal(res.Content)
	}
	if !strings.Contains(res.Content, "no matches") {
		t.Errorf("expected 'no matches', got %q", res.Content)
	}
}

// ─── Plugin tool roundtrip ────────────────────────────────────────────────────

func TestPluginTool_RoundTrip_FromPluginJSON(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("plugin test uses shell scripts; skipping on Windows")
	}

	dir := t.TempDir()
	pluginDir := filepath.Join(dir, "myplugin")
	os.Mkdir(pluginDir, 0o755)

	// Write fake plugin.json with two commands.
	pj := `{
		"name": "myplugin",
		"binary": "echo",
		"capabilities": {
			"commands": {
				"greet": {
					"description": "Say hello",
					"args": ["--name <name>"]
				},
				"ping": {
					"description": "Ping the service",
					"args": []
				}
			}
		}
	}`
	os.WriteFile(filepath.Join(pluginDir, "plugin.json"), []byte(pj), 0o644)

	tools := pluginTools(context.Background(), dir)
	if len(tools) < 2 {
		t.Fatalf("expected >=2 tools from plugin.json, got %d", len(tools))
	}

	// Verify schema validity for each generated tool.
	for _, tool := range tools {
		schemaValid(t, tool.Name(), tool.InputSchema())
	}

	// Find the greet tool and verify its schema has the 'name' property.
	var greet Tool
	for _, tool := range tools {
		if strings.HasSuffix(tool.Name(), "_greet") {
			greet = tool
			break
		}
	}
	if greet == nil {
		t.Fatal("myplugin_greet not found in generated tools")
	}

	var doc struct {
		Properties map[string]any `json:"properties"`
	}
	json.Unmarshal(greet.InputSchema(), &doc)
	if _, ok := doc.Properties["name"]; !ok {
		t.Errorf("greet schema missing 'name' property; props=%v", doc.Properties)
	}
}

func TestPluginTool_RoundTrip_WithFakeBinary(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("fake binary uses shell script; skipping on Windows")
	}

	dir := t.TempDir()
	binDir := filepath.Join(dir, "bin")
	os.Mkdir(binDir, 0o755)

	// Create a fake plugin binary that responds to --help-json.
	script := `#!/bin/sh
case "$1" in
  --help-json)
    cat <<'EOF'
{"name":"fakeplugin","commands":[{"name":"echo","description":"Echo input","args":[{"name":"message","type":"string","required":true}]}]}
EOF
    ;;
  echo)
    echo "ECHO: $3"
    ;;
  *)
    echo "unknown command"
    exit 1
    ;;
esac
`
	binPath := filepath.Join(binDir, "fakeplugin")
	os.WriteFile(binPath, []byte(script), 0o755)

	// Write plugin.json pointing to the fake binary.
	pluginDir := filepath.Join(dir, "fakeplugin")
	os.Mkdir(pluginDir, 0o755)
	pj := fmt.Sprintf(`{"name":"fakeplugin","binary":"%s","capabilities":{"commands":{}}}`, binPath)
	os.WriteFile(filepath.Join(pluginDir, "plugin.json"), []byte(pj), 0o644)

	tools := pluginTools(context.Background(), dir)
	if len(tools) == 0 {
		t.Fatal("expected at least one tool from --help-json")
	}

	var echoTool Tool
	for _, tool := range tools {
		if tool.Name() == "fakeplugin_echo" {
			echoTool = tool
			break
		}
	}
	if echoTool == nil {
		t.Fatalf("fakeplugin_echo not found; got tools: %v", toolNames(tools))
	}

	schemaValid(t, echoTool.Name(), echoTool.InputSchema())

	// Execute the tool; --help-json reported "message" as a required string arg.
	res, err := echoTool.Execute(context.Background(), map[string]any{"message": "world"})
	if err != nil {
		t.Fatal(err)
	}
	// The fake binary echoes back — check it ran without crashing.
	_ = res
}

// ─── Registry ────────────────────────────────────────────────────────────────

func TestRegistry_TierOneTools(t *testing.T) {
	tools := tier1Tools()
	if len(tools) < 6 {
		t.Errorf("expected >=6 tier-1 tools, got %d", len(tools))
	}
	names := map[string]bool{}
	for _, tool := range tools {
		names[tool.Name()] = true
		schemaValid(t, tool.Name(), tool.InputSchema())
	}
	for _, required := range []string{"bash", "file_read", "file_write", "file_edit", "glob", "grep"} {
		if !names[required] {
			t.Errorf("missing required tool: %q", required)
		}
	}
}

func TestRegistry_New(t *testing.T) {
	r := New()
	if r.Len() != 0 {
		t.Errorf("new registry should be empty, got %d", r.Len())
	}
}

func TestRegistry_Register_Duplicate(t *testing.T) {
	r := New()
	r.Register(NewBashTool())
	defer func() {
		if rec := recover(); rec == nil {
			t.Error("expected panic on duplicate registration")
		}
	}()
	r.Register(NewBashTool())
}

func TestRegistry_Get(t *testing.T) {
	r := New()
	r.Register(NewBashTool())
	if r.Get("bash") == nil {
		t.Error("Get(bash) returned nil after registration")
	}
	if r.Get("nonexistent") != nil {
		t.Error("Get(nonexistent) should return nil")
	}
}

func TestRegistry_All_Sorted(t *testing.T) {
	r := New()
	r.Register(NewGlobTool())
	r.Register(NewBashTool())
	r.Register(NewGrepTool())
	all := r.All()
	for i := 1; i < len(all); i++ {
		if all[i].Name() < all[i-1].Name() {
			t.Errorf("All() not sorted: %q before %q", all[i-1].Name(), all[i].Name())
		}
	}
}

// ─── RiskTier ────────────────────────────────────────────────────────────────

func TestRiskTier_String(t *testing.T) {
	cases := []struct {
		tier RiskTier
		want string
	}{
		{RiskLow, "low"},
		{RiskMedium, "medium"},
		{RiskHigh, "high"},
		{RiskCritical, "critical"},
	}
	for _, c := range cases {
		if got := c.tier.String(); got != c.want {
			t.Errorf("RiskTier(%d).String() = %q, want %q", c.tier, got, c.want)
		}
	}
}

func TestRiskTiers_Correct(t *testing.T) {
	cases := map[string]RiskTier{
		"bash":       RiskHigh,
		"file_read":  RiskLow,
		"file_write": RiskMedium,
		"file_edit":  RiskMedium,
		"glob":       RiskLow,
		"grep":       RiskLow,
	}
	for _, tool := range tier1Tools() {
		want, ok := cases[tool.Name()]
		if !ok {
			continue
		}
		if tool.Risk() != want {
			t.Errorf("%s.Risk() = %v, want %v", tool.Name(), tool.Risk(), want)
		}
	}
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

func writeTemp(t *testing.T, content string) string {
	t.Helper()
	f, err := os.CreateTemp(t.TempDir(), "test_*")
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	f.WriteString(content)
	return f.Name()
}

func touch(t *testing.T, path string) {
	t.Helper()
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

func toolNames(tools []Tool) []string {
	var names []string
	for _, t := range tools {
		names = append(names, t.Name())
	}
	return names
}

// Compile-time check: BashTool implements Streamer.
var _ Streamer = (*BashTool)(nil)

// Compile-time check: all tier-1 tools implement Tool.
var _ Tool = (*BashTool)(nil)
var _ Tool = (*FileReadTool)(nil)
var _ Tool = (*FileWriteTool)(nil)
var _ Tool = (*FileEditTool)(nil)
var _ Tool = (*GlobTool)(nil)
var _ Tool = (*GrepTool)(nil)
