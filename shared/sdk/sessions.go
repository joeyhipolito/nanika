// Package sdk provides a Go SDK for communicating with Claude Code CLI.
//
// Session schema reference: shared/artifacts/b3-claude-jsonl-schema.md
// Each ~/.claude/projects/<slug>/<uuid>.jsonl file contains newline-delimited JSON records.
// Records are discriminated by top-level "type": user | assistant | attachment |
// queue-operation | last-prompt. Slug encoding: replace every '/' and '.' in the
// cwd with '-'.
package sdk

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

// ErrInvalidSessionID is returned when a session ID fails validation.
var ErrInvalidSessionID = errors.New("sdk: invalid session ID")

// ErrSessionNotFound is returned when a session ID is valid but no matching file exists.
var ErrSessionNotFound = errors.New("sdk: session not found")

// sessionsRoot is the base directory scanned for session JSONL files.
// Tests override this via setSessionsRoot.
var sessionsRoot = projectsDir()

func projectsDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".claude", "projects")
}

var sessionIDRe = regexp.MustCompile(`^[a-zA-Z0-9-]+$`)

// validateSessionID rejects session IDs containing path traversal sequences,
// absolute paths, slashes, or characters outside [a-zA-Z0-9-].
func validateSessionID(id string) error {
	if id == "" || id == ".." || strings.Contains(id, "/") || filepath.IsAbs(id) || !sessionIDRe.MatchString(id) {
		return fmt.Errorf("%w: %q", ErrInvalidSessionID, id)
	}
	return nil
}

// SessionInfo holds summary metadata for a Claude Code session.
type SessionInfo struct {
	ID               string    // UUID (filename stem)
	ProjectSlug      string    // subdirectory name under ~/.claude/projects/
	CreatedAt        time.Time // timestamp of first record
	UpdatedAt        time.Time // timestamp of last record
	NumTurns         int       // number of user+assistant record pairs
	CWD              string    // working directory from first record
	GitBranch        string    // git branch from first record
	Version          string    // Claude Code version from first record
	FirstUserMessage string    // text preview of the first user turn (truncated at 100 runes)
}

// SessionMessage is a single conversation turn from a session file.
// Only user and assistant records are included; metadata records are excluded.
type SessionMessage struct {
	UUID       string          `json:"uuid"`
	ParentUUID string          `json:"parentUuid"`
	Type       string          `json:"type"` // "user" or "assistant"
	Timestamp  time.Time       `json:"timestamp"`
	Message    json.RawMessage `json:"message"`
}

// sessionRecord is the minimal envelope decoded from each JSONL line.
type sessionRecord struct {
	Type       string  `json:"type"`
	UUID       string  `json:"uuid"`
	ParentUUID string  `json:"parentUuid"`
	SessionID  string  `json:"sessionId"`
	Timestamp  string  `json:"timestamp"`
	CWD        string  `json:"cwd"`
	GitBranch  string  `json:"gitBranch"`
	Version    string  `json:"version"`
	Message    json.RawMessage `json:"message"`
}

// ListSessions returns all sessions found under sessionsRoot, sorted by UpdatedAt descending.
func ListSessions() ([]SessionInfo, error) {
	root := sessionsRoot
	if root == "" {
		return nil, errors.New("sdk: sessions root is empty (could not determine home dir)")
	}

	slugDirs, err := os.ReadDir(root)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading sessions root %s: %w", root, err)
	}

	var sessions []SessionInfo
	for _, slugEntry := range slugDirs {
		if !slugEntry.IsDir() {
			continue
		}
		slug := slugEntry.Name()
		slugPath := filepath.Join(root, slug)

		entries, err := os.ReadDir(slugPath)
		if err != nil {
			continue
		}
		for _, entry := range entries {
			if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
				continue
			}
			id := strings.TrimSuffix(entry.Name(), ".jsonl")
			info, err := readSessionInfo(slugPath, id, slug)
			if err != nil {
				continue
			}
			sessions = append(sessions, info)
		}
	}

	// Sort descending by UpdatedAt.
	for i := 1; i < len(sessions); i++ {
		for j := i; j > 0 && sessions[j].UpdatedAt.After(sessions[j-1].UpdatedAt); j-- {
			sessions[j], sessions[j-1] = sessions[j-1], sessions[j]
		}
	}

	return sessions, nil
}

// GetSessionInfo returns metadata for the session with the given ID.
// The ID must be a UUID-like string ([a-zA-Z0-9-]+).
func GetSessionInfo(id string) (SessionInfo, error) {
	if err := validateSessionID(id); err != nil {
		return SessionInfo{}, err
	}

	root := sessionsRoot
	slug, err := findSessionSlug(root, id)
	if err != nil {
		return SessionInfo{}, err
	}

	return readSessionInfo(filepath.Join(root, slug), id, slug)
}

// GetSessionMessages returns the user and assistant turns for the given session ID.
// Attachment, queue-operation, and last-prompt records are excluded.
func GetSessionMessages(id string) ([]SessionMessage, error) {
	if err := validateSessionID(id); err != nil {
		return nil, err
	}

	root := sessionsRoot
	slug, err := findSessionSlug(root, id)
	if err != nil {
		return nil, err
	}

	path := filepath.Join(root, slug, id+".jsonl")
	return readSessionMessages(path)
}

// DeleteSession removes the JSONL file for the given session ID.
// It is idempotent: if the session does not exist, it returns nil.
// It does not remove the slug directory even if it becomes empty.
func DeleteSession(id string) error {
	if err := validateSessionID(id); err != nil {
		return err
	}

	root := sessionsRoot
	slug, err := findSessionSlug(root, id)
	if err != nil {
		if errors.Is(err, ErrSessionNotFound) {
			return nil
		}
		return err
	}

	path := filepath.Join(root, slug, id+".jsonl")
	if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("deleting session %s: %w", id, err)
	}
	return nil
}

// findSessionSlug scans all slug subdirectories under root for <id>.jsonl and
// returns the slug name that contains it.
func findSessionSlug(root, id string) (string, error) {
	if root == "" {
		return "", errors.New("sdk: sessions root is empty (could not determine home dir)")
	}

	slugDirs, err := os.ReadDir(root)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return "", fmt.Errorf("%w: %s (sessions root does not exist)", ErrSessionNotFound, id)
		}
		return "", fmt.Errorf("reading sessions root: %w", err)
	}

	for _, entry := range slugDirs {
		if !entry.IsDir() {
			continue
		}
		candidate := filepath.Join(root, entry.Name(), id+".jsonl")
		if _, err := os.Stat(candidate); err == nil {
			return entry.Name(), nil
		}
	}
	return "", fmt.Errorf("%w: %s", ErrSessionNotFound, id)
}

// readSessionInfo reads a JSONL file and builds a SessionInfo from its records.
func readSessionInfo(slugDir, id, slug string) (SessionInfo, error) {
	path := filepath.Join(slugDir, id+".jsonl")
	f, err := os.Open(path)
	if err != nil {
		return SessionInfo{}, fmt.Errorf("opening session %s: %w", id, err)
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 64*1024), 10*1024*1024)

	info := SessionInfo{
		ID:          id,
		ProjectSlug: slug,
	}

	var firstTimestamp, lastTimestamp time.Time
	var userCount, assistantCount int

	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		var rec sessionRecord
		if err := json.Unmarshal(line, &rec); err != nil {
			continue
		}

		if rec.Timestamp != "" {
			ts, err := time.Parse(time.RFC3339Nano, rec.Timestamp)
			if err == nil {
				if firstTimestamp.IsZero() || ts.Before(firstTimestamp) {
					firstTimestamp = ts
				}
				if lastTimestamp.IsZero() || ts.After(lastTimestamp) {
					lastTimestamp = ts
				}
			}
		}

		if info.CWD == "" && rec.CWD != "" {
			info.CWD = rec.CWD
		}
		if info.GitBranch == "" && rec.GitBranch != "" {
			info.GitBranch = rec.GitBranch
		}
		if info.Version == "" && rec.Version != "" {
			info.Version = rec.Version
		}

		switch rec.Type {
		case "user":
			userCount++
			if info.FirstUserMessage == "" && len(rec.Message) > 0 {
				info.FirstUserMessage = extractMessagePreview(rec.Message)
			}
		case "assistant":
			assistantCount++
		}
	}
	if err := scanner.Err(); err != nil {
		return SessionInfo{}, fmt.Errorf("scanning session %s: %w", id, err)
	}

	info.CreatedAt = firstTimestamp
	info.UpdatedAt = lastTimestamp
	// NumTurns = min(userCount, assistantCount) since each turn is a matched pair.
	info.NumTurns = userCount
	if assistantCount < userCount {
		info.NumTurns = assistantCount
	}

	return info, nil
}

// readSessionMessages reads user and assistant records from a JSONL file.
func readSessionMessages(path string) ([]SessionMessage, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("opening session file: %w", err)
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 64*1024), 10*1024*1024)

	var messages []SessionMessage
	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		var rec sessionRecord
		if err := json.Unmarshal(line, &rec); err != nil {
			continue
		}
		if rec.Type != "user" && rec.Type != "assistant" {
			continue
		}

		msg := SessionMessage{
			UUID:       rec.UUID,
			ParentUUID: rec.ParentUUID,
			Type:       rec.Type,
			Message:    rec.Message,
		}
		if rec.Timestamp != "" {
			ts, err := time.Parse(time.RFC3339Nano, rec.Timestamp)
			if err == nil {
				msg.Timestamp = ts
			}
		}
		messages = append(messages, msg)
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("scanning session file: %w", err)
	}
	return messages, nil
}

// extractMessagePreview returns up to 100 runes of plain text from a user message JSON blob.
// The message content may be a bare string or an object with a "content" field.
func extractMessagePreview(raw json.RawMessage) string {
	// Try object form: {"role":"user","content":"..."}
	var body struct {
		Content json.RawMessage `json:"content"`
	}
	if err := json.Unmarshal(raw, &body); err == nil && len(body.Content) > 0 {
		// content is a plain string
		var s string
		if err := json.Unmarshal(body.Content, &s); err == nil {
			return truncateRunes(s, 100)
		}
	}
	// Fallback: treat the whole blob as a string
	var s string
	if err := json.Unmarshal(raw, &s); err == nil {
		return truncateRunes(s, 100)
	}
	return ""
}

func truncateRunes(s string, n int) string {
	runes := []rune(s)
	if len(runes) <= n {
		return s
	}
	return string(runes[:n])
}
