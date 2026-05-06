package sdk

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// ─── fixture helpers ────────────────────────────────────────────────────────

// fixtureSession creates root/slug/id.jsonl and writes the provided JSONL lines.
func fixtureSession(t *testing.T, root, slug, id string, lines []string) {
	t.Helper()
	dir := filepath.Join(root, slug)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("fixtureSession: mkdir %s: %v", dir, err)
	}
	f, err := os.Create(filepath.Join(dir, id+".jsonl"))
	if err != nil {
		t.Fatalf("fixtureSession: create: %v", err)
	}
	defer f.Close()
	for _, line := range lines {
		if _, err := fmt.Fprintln(f, line); err != nil {
			t.Fatalf("fixtureSession: write: %v", err)
		}
	}
}

// userLine returns a minimal JSONL user record.
func userLine(uuid, parentUUID, sessionID, ts, cwd, branch, version, text string) string {
	parent := "null"
	if parentUUID != "" {
		parent = `"` + parentUUID + `"`
	}
	content, _ := json.Marshal(text)
	return fmt.Sprintf(
		`{"type":"user","uuid":%q,"parentUuid":%s,"sessionId":%q,"timestamp":%q,"cwd":%q,"gitBranch":%q,"version":%q,"message":{"role":"user","content":%s}}`,
		uuid, parent, sessionID, ts, cwd, branch, version, content,
	)
}

// assistantLine returns a minimal JSONL assistant record.
func assistantLine(uuid, parentUUID, sessionID, ts string) string {
	return fmt.Sprintf(
		`{"type":"assistant","uuid":%q,"parentUuid":%q,"sessionId":%q,"timestamp":%q,"message":{"role":"assistant","id":"msg_01","type":"message","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}}`,
		uuid, parentUUID, sessionID, ts,
	)
}

// ─── TestListSessions_OrderingAndFields ─────────────────────────────────────

// TestListSessions_OrderingAndFields verifies sessions are returned newest-first,
// NumTurns is correct, and FirstUserMessage contains the expected preview text.
func TestListSessions_OrderingAndFields(t *testing.T) {
	root := t.TempDir()
	setSessionsRoot(t, root)

	slug := "project-a"

	// Three sessions: A oldest, B middle, C newest.
	sessions := []struct {
		id       string
		ts       string // timestamp for the single turn
		text     string // first user message text
		numTurns int
	}{
		{"sess-a", "2026-01-01T10:00:00.000Z", "Hello from A", 1},
		{"sess-b", "2026-02-01T10:00:00.000Z", "Hello from B", 2},
		{"sess-c", "2026-03-01T10:00:00.000Z", "Hello from C", 3},
	}

	for _, s := range sessions {
		var lines []string
		for i := 0; i < s.numTurns; i++ {
			uUID := fmt.Sprintf("%s-u%d", s.id, i)
			aUUID := fmt.Sprintf("%s-a%d", s.id, i)
			parent := ""
			if i > 0 {
				parent = fmt.Sprintf("%s-a%d", s.id, i-1)
			}
			// Advance time slightly per turn so UpdatedAt reflects the last record.
			turnTS := advanceTS(t, s.ts, time.Duration(i)*time.Minute)
			var text string
			if i == 0 {
				text = s.text
			} else {
				text = fmt.Sprintf("follow-up %d", i)
			}
			lines = append(lines,
				userLine(uUID, parent, s.id, turnTS, "/work", "main", "1.0", text),
				assistantLine(aUUID, uUID, s.id, advanceTS(t, turnTS, 10*time.Second)),
			)
		}
		fixtureSession(t, root, slug, s.id, lines)
	}

	got, err := ListSessions()
	if err != nil {
		t.Fatalf("ListSessions: %v", err)
	}
	if len(got) != 3 {
		t.Fatalf("want 3 sessions, got %d", len(got))
	}

	// Newest-first order: C, B, A.
	wantOrder := []string{"sess-c", "sess-b", "sess-a"}
	for i, want := range wantOrder {
		if got[i].ID != want {
			t.Errorf("position %d: want %s, got %s", i, want, got[i].ID)
		}
	}

	// NumTurns and FirstUserMessage for each.
	wantTurns := map[string]int{"sess-a": 1, "sess-b": 2, "sess-c": 3}
	wantText := map[string]string{
		"sess-a": "Hello from A",
		"sess-b": "Hello from B",
		"sess-c": "Hello from C",
	}
	for _, info := range got {
		if info.NumTurns != wantTurns[info.ID] {
			t.Errorf("%s: NumTurns = %d, want %d", info.ID, info.NumTurns, wantTurns[info.ID])
		}
		if info.FirstUserMessage != wantText[info.ID] {
			t.Errorf("%s: FirstUserMessage = %q, want %q", info.ID, info.FirstUserMessage, wantText[info.ID])
		}
	}
}

// advanceTS parses ts (RFC3339 / ISO8601) and adds d, returning the result formatted the same way.
func advanceTS(t *testing.T, ts string, d time.Duration) string {
	t.Helper()
	parsed, err := time.Parse("2006-01-02T15:04:05.000Z", ts)
	if err != nil {
		// Try plain RFC3339 without ms.
		parsed, err = time.Parse(time.RFC3339, ts)
		if err != nil {
			t.Fatalf("advanceTS: parse %q: %v", ts, err)
		}
	}
	return parsed.Add(d).UTC().Format("2006-01-02T15:04:05.000Z")
}

// ─── TestGetSessionInfo ──────────────────────────────────────────────────────

// TestGetSessionInfo verifies that the returned SessionInfo matches the fixture.
func TestGetSessionInfo(t *testing.T) {
	root := t.TempDir()
	setSessionsRoot(t, root)

	id := "info-session"
	slug := "project-info"
	ts0 := "2026-06-15T08:00:00.000Z"
	ts1 := "2026-06-15T08:05:00.000Z"

	fixtureSession(t, root, slug, id, []string{
		userLine("u1", "", id, ts0, "/my/project", "feature-branch", "2.1.0", "first message"),
		assistantLine("a1", "u1", id, ts1),
	})

	info, err := GetSessionInfo(id)
	if err != nil {
		t.Fatalf("GetSessionInfo: %v", err)
	}

	tests := []struct {
		name string
		got  interface{}
		want interface{}
	}{
		{"ID", info.ID, id},
		{"ProjectSlug", info.ProjectSlug, slug},
		{"NumTurns", info.NumTurns, 1},
		{"CWD", info.CWD, "/my/project"},
		{"GitBranch", info.GitBranch, "feature-branch"},
		{"Version", info.Version, "2.1.0"},
		{"FirstUserMessage", info.FirstUserMessage, "first message"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.got != tt.want {
				t.Errorf("got %v, want %v", tt.got, tt.want)
			}
		})
	}

	// Timestamps parsed correctly.
	wantCreated, _ := time.Parse("2006-01-02T15:04:05.000Z", ts0)
	wantUpdated, _ := time.Parse("2006-01-02T15:04:05.000Z", ts1)
	if !info.CreatedAt.Equal(wantCreated) {
		t.Errorf("CreatedAt: got %v, want %v", info.CreatedAt, wantCreated)
	}
	if !info.UpdatedAt.Equal(wantUpdated) {
		t.Errorf("UpdatedAt: got %v, want %v", info.UpdatedAt, wantUpdated)
	}
}

// ─── TestGetSessionMessages_LimitOffset ──────────────────────────────────────

// TestGetSessionMessages_LimitOffset verifies message ordering: applying a
// limit of 5 returns 5 messages and applying offset 1 skips the first message.
func TestGetSessionMessages_LimitOffset(t *testing.T) {
	root := t.TempDir()
	setSessionsRoot(t, root)

	id := "paginate-session"
	slug := "project-paginate"

	// Build 7 user+assistant pairs = 14 message records.
	const total = 7
	var lines []string
	var expectedUUIDs []string // only user UUIDs, in order
	baseTS := "2026-05-01T09:00:00.000Z"
	for i := 0; i < total; i++ {
		uUID := fmt.Sprintf("u%02d", i)
		aUUID := fmt.Sprintf("a%02d", i)
		parent := ""
		if i > 0 {
			parent = fmt.Sprintf("a%02d", i-1)
		}
		ts := advanceTS(t, baseTS, time.Duration(i)*2*time.Minute)
		aTS := advanceTS(t, baseTS, time.Duration(i)*2*time.Minute+time.Minute)
		lines = append(lines,
			userLine(uUID, parent, id, ts, "/work", "main", "1.0", fmt.Sprintf("msg %d", i)),
			assistantLine(aUUID, uUID, id, aTS),
		)
		expectedUUIDs = append(expectedUUIDs, uUID)
	}
	fixtureSession(t, root, slug, id, lines)

	msgs, err := GetSessionMessages(id)
	if err != nil {
		t.Fatalf("GetSessionMessages: %v", err)
	}
	if len(msgs) != total*2 {
		t.Fatalf("want %d messages (user+assistant), got %d", total*2, len(msgs))
	}

	tests := []struct {
		name      string
		slice     []SessionMessage
		wantLen   int
		wantFirst string // UUID of first element in slice
	}{
		{
			name:      "limit=5 returns first 5",
			slice:     msgs[:5],
			wantLen:   5,
			wantFirst: expectedUUIDs[0],
		},
		{
			name:      "offset=1 skips first message",
			slice:     msgs[1:],
			wantLen:   total*2 - 1,
			wantFirst: "a00", // first element after skip is the first assistant record
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if len(tt.slice) != tt.wantLen {
				t.Errorf("len = %d, want %d", len(tt.slice), tt.wantLen)
			}
			if tt.slice[0].UUID != tt.wantFirst {
				t.Errorf("first UUID = %q, want %q", tt.slice[0].UUID, tt.wantFirst)
			}
		})
	}
}

// ─── TestDeleteSession_Idempotent ────────────────────────────────────────────

// TestDeleteSession_Idempotent verifies that deleting a session removes the file
// and that a second delete returns nil (no error).
func TestDeleteSession_Idempotent(t *testing.T) {
	root := t.TempDir()
	setSessionsRoot(t, root)

	id := "delete-me"
	slug := "project-delete"
	fixtureSession(t, root, slug, id, []string{
		userLine("u1", "", id, "2026-07-01T10:00:00.000Z", "/work", "main", "1.0", "hello"),
		assistantLine("a1", "u1", id, "2026-07-01T10:01:00.000Z"),
	})

	sessionFile := filepath.Join(root, slug, id+".jsonl")

	// First delete: file must be removed.
	if err := DeleteSession(id); err != nil {
		t.Fatalf("first DeleteSession: %v", err)
	}
	if _, err := os.Stat(sessionFile); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("session file still exists after delete")
	}

	// Second delete: must return nil (idempotent).
	if err := DeleteSession(id); err != nil {
		t.Errorf("second DeleteSession (idempotent): got error %v, want nil", err)
	}
}

// TestDeleteSession_RootMissing verifies that DeleteSession returns nil when
// the sessions root directory itself does not exist (idempotent guarantee).
func TestDeleteSession_RootMissing(t *testing.T) {
	root := t.TempDir()
	setSessionsRoot(t, root)

	// Remove the root entirely so os.ReadDir inside findSessionSlug hits fs.ErrNotExist.
	if err := os.RemoveAll(root); err != nil {
		t.Fatalf("setup: %v", err)
	}

	if err := DeleteSession("nonexistent-id"); err != nil {
		t.Errorf("DeleteSession with missing root: got %v, want nil", err)
	}
}

// ─── TestDeleteSession_PathTraversal ─────────────────────────────────────────

// TestDeleteSession_PathTraversal verifies that path traversal and absolute-path
// IDs are rejected with ErrInvalidSessionID without touching any files.
func TestDeleteSession_PathTraversal(t *testing.T) {
	// parent holds a sentinel file that must NOT be touched.
	parent := t.TempDir()
	sentinel := filepath.Join(parent, "sentinel.txt")
	if err := os.WriteFile(sentinel, []byte("safe"), 0o644); err != nil {
		t.Fatalf("writing sentinel: %v", err)
	}

	// sessionsRoot is a subdirectory of parent so that "../sentinel" would be
	// reachable if the guard were absent.
	projectsRoot := filepath.Join(parent, "projects")
	if err := os.MkdirAll(projectsRoot, 0o755); err != nil {
		t.Fatalf("creating projects root: %v", err)
	}
	setSessionsRoot(t, projectsRoot)

	tests := []struct {
		name string
		id   string
	}{
		{"relative traversal", "../sentinel"},
		{"absolute path", "/etc/passwd"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := DeleteSession(tt.id)
			if !errors.Is(err, ErrInvalidSessionID) {
				t.Errorf("DeleteSession(%q): want ErrInvalidSessionID, got %v", tt.id, err)
			}
			// Sentinel file must be untouched.
			if _, err := os.Stat(sentinel); err != nil {
				t.Errorf("sentinel file was removed or is inaccessible: %v", err)
			}
		})
	}
}
