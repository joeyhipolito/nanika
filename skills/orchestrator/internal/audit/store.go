package audit

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

// auditStorePathFunc is the function that returns the audit store path.
// Overridable in tests.
var auditStorePathFunc = defaultAuditStorePath

func defaultAuditStorePath() (string, error) {
	base, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("cannot determine config directory: %w", err)
	}
	return filepath.Join(base, "audits.jsonl"), nil
}

// SaveReport appends an audit report as a JSONL line to ~/.alluka/audits.jsonl.
func SaveReport(report *AuditReport) error {
	path, err := auditStorePathFunc()
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return fmt.Errorf("creating audits dir: %w", err)
	}

	data, err := json.Marshal(report)
	if err != nil {
		return fmt.Errorf("marshaling report: %w", err)
	}
	data = append(data, '\n')

	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return fmt.Errorf("opening audits file: %w", err)
	}
	defer f.Close()

	_, err = f.Write(data)
	return err
}

// LoadReports reads all audit reports from ~/.alluka/audits.jsonl.
// Returns in append order (oldest first). Returns nil, nil if file doesn't exist.
func LoadReports() ([]AuditReport, error) {
	path, err := auditStorePathFunc()
	if err != nil {
		return nil, err
	}

	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("opening audits file: %w", err)
	}
	defer f.Close()

	var reports []AuditReport
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for scanner.Scan() {
		var r AuditReport
		if err := json.Unmarshal(scanner.Bytes(), &r); err != nil {
			continue // skip malformed lines
		}
		reports = append(reports, r)
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading audits file: %w", err)
	}

	return reports, nil
}
