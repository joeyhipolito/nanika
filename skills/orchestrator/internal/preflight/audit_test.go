package preflight

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// TestAuditLogPath verifies the daily-rotating path format.
// The file name must encode the UTC date so midnight rollover is deterministic
// regardless of the caller's local timezone.
func TestAuditLogPath(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	// Use a local-time input to assert the helper normalizes to UTC before
	// formatting. 23:30 Pacific on 2026-04-19 is 2026-04-20 UTC.
	loc, err := time.LoadLocation("America/Los_Angeles")
	if err != nil {
		t.Skipf("timezone data unavailable: %v", err)
	}
	local := time.Date(2026, 4, 19, 23, 30, 0, 0, loc)

	got, err := auditLogPath(local)
	if err != nil {
		t.Fatalf("auditLogPath: %v", err)
	}
	want := filepath.Join(home, ".alluka", "logs", "preflight-audit.2026-04-20.jsonl")
	if got != want {
		t.Fatalf("auditLogPath:\n got  %s\n want %s", got, want)
	}
}

// TestWriteAudit_AppendsJSONL verifies that WriteAudit creates the log
// directory (0700), writes a well-formed JSONL record, and appends on repeat
// calls without clobbering prior entries.
func TestWriteAudit_AppendsJSONL(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	now := time.Date(2026, 4, 20, 12, 0, 0, 0, time.UTC)
	entry1 := AuditEntry{
		Timestamp:        now,
		SectionsIncluded: []string{"scheduler", "tracker"},
		SectionsDropped:  []string{"learnings"},
		RenderedBytes:    1234,
		MaxBytes:         6144,
		Format:           "text",
	}
	WriteAudit(entry1)

	entry2 := AuditEntry{
		Timestamp:        now.Add(time.Minute),
		SectionsIncluded: []string{"scheduler"},
		RenderedBytes:    321,
		MaxBytes:         6144,
		Format:           "text",
	}
	WriteAudit(entry2)

	path := filepath.Join(home, ".alluka", "logs", "preflight-audit.2026-04-20.jsonl")
	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("log file not created: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0600 {
		t.Errorf("log file perm = %04o, want 0600", perm)
	}

	dirInfo, err := os.Stat(filepath.Dir(path))
	if err != nil {
		t.Fatalf("log dir not created: %v", err)
	}
	if perm := dirInfo.Mode().Perm(); perm != 0700 {
		t.Errorf("log dir perm = %04o, want 0700", perm)
	}

	f, err := os.Open(path)
	if err != nil {
		t.Fatalf("open log: %v", err)
	}
	defer f.Close()

	var lines []AuditEntry
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" {
			continue
		}
		var e AuditEntry
		if err := json.Unmarshal([]byte(line), &e); err != nil {
			t.Fatalf("unmarshal line %q: %v", line, err)
		}
		lines = append(lines, e)
	}
	if err := sc.Err(); err != nil {
		t.Fatalf("scan: %v", err)
	}

	if len(lines) != 2 {
		t.Fatalf("want 2 entries after two WriteAudit calls, got %d", len(lines))
	}
	if lines[0].RenderedBytes != 1234 || lines[1].RenderedBytes != 321 {
		t.Errorf("append order wrong: %+v", lines)
	}
	if lines[0].SectionsDropped[0] != "learnings" {
		t.Errorf("dropped slice not round-tripped: %+v", lines[0].SectionsDropped)
	}
}

// TestWriteAudit_BestEffort_HomeMissing verifies the writer never panics or
// returns an error path when home discovery fails. This matches the contract
// used in cmd/hooks.go: audit logging must not break the preflight hook.
func TestWriteAudit_BestEffort_HomeMissing(t *testing.T) {
	// Clear HOME so os.UserHomeDir fails on darwin/linux.
	t.Setenv("HOME", "")
	t.Setenv("USERPROFILE", "") // Windows fallback

	// Any panic here would fail the test — WriteAudit must swallow it.
	WriteAudit(AuditEntry{
		Timestamp:     time.Now().UTC(),
		RenderedBytes: 1,
		Format:        "text",
	})
}

// TestWriteAudit_ZeroTimestampFilledIn verifies that a zero Timestamp is
// automatically filled with the current UTC time before writing, so the log
// file is always chosen by a deterministic date.
func TestWriteAudit_ZeroTimestampFilledIn(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	WriteAudit(AuditEntry{
		SectionsIncluded: []string{"tracker"},
		RenderedBytes:    42,
		Format:           "text",
	})

	// Discover the single file written under ~/.alluka/logs.
	logDir := filepath.Join(home, ".alluka", "logs")
	entries, err := os.ReadDir(logDir)
	if err != nil {
		t.Fatalf("readdir: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("want exactly 1 audit file, got %d", len(entries))
	}
	name := entries[0].Name()
	if !strings.HasPrefix(name, "preflight-audit.") || !strings.HasSuffix(name, ".jsonl") {
		t.Errorf("unexpected audit file name: %s", name)
	}
}
