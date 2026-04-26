package preflight

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// AuditEntry is one record written to the preflight audit log. The log
// captures which sections were assembled into a brief, which were dropped
// to fit the byte budget, and the final rendered size — so SessionStart
// retrieval decisions can be reconstructed offline.
type AuditEntry struct {
	Timestamp        time.Time `json:"ts"`
	SectionsIncluded []string  `json:"sections_built"`
	SectionsDropped  []string  `json:"sections_dropped"`
	RenderedBytes    int       `json:"total_bytes"`
	MaxBytes         int       `json:"budget_bytes"`
	Format           string    `json:"format"`
	DropReason       string    `json:"drop_reason,omitempty"`
}

// auditLogPath returns the daily-rotating audit log path for the given time.
// The file lives under ~/.alluka/logs/preflight-audit.<YYYY-MM-DD>.jsonl so
// logs roll over at UTC midnight and stay a fixed count per day.
func auditLogPath(now time.Time) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("home dir: %w", err)
	}
	name := fmt.Sprintf("preflight-audit.%s.jsonl", now.UTC().Format("2006-01-02"))
	return filepath.Join(home, ".alluka", "logs", name), nil
}

// WriteAudit appends a JSONL-encoded entry to the daily audit log. Any
// failure (home lookup, mkdir, open, marshal, write) is swallowed — the
// audit log is best-effort and must never break the preflight hook.
func WriteAudit(entry AuditEntry) {
	if entry.Timestamp.IsZero() {
		entry.Timestamp = time.Now().UTC()
	}
	path, err := auditLogPath(entry.Timestamp)
	if err != nil {
		return
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return
	}
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return
	}
	defer f.Close()

	data, err := json.Marshal(entry)
	if err != nil {
		return
	}
	data = append(data, '\n')
	_, _ = f.Write(data)
}
