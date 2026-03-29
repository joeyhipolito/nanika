// Package scan provides shared types and utilities for nanika-nen scanner binaries.
// All three scanners (gyo, en, ryu) import this package for Finding types,
// config resolution, and database helpers.
package scan

import (
	"context"
	"time"
)

// Severity classifies the urgency of a finding.
type Severity string

const (
	SeverityCritical Severity = "critical"
	SeverityHigh     Severity = "high"
	SeverityMedium   Severity = "medium"
	SeverityLow      Severity = "low"
	SeverityInfo     Severity = "info"
)

// Scope defines what a finding targets.
type Scope struct {
	Kind  string `json:"kind"`
	Value string `json:"value"`
}

// Evidence is a piece of supporting data attached to a finding.
type Evidence struct {
	Kind       string    `json:"kind"`
	Raw        string    `json:"raw"`
	Source     string    `json:"source"`
	CapturedAt time.Time `json:"captured_at"`
}

// Finding is a single intelligence or security finding produced by a scanner.
type Finding struct {
	ID           string     `json:"id"`
	Ability      string     `json:"ability"`
	Category     string     `json:"category"`
	Severity     Severity   `json:"severity"`
	Title        string     `json:"title"`
	Description  string     `json:"description"`
	Scope        Scope      `json:"scope"`
	Evidence     []Evidence `json:"evidence,omitempty"`
	Source       string     `json:"source"`
	FoundAt      time.Time  `json:"found_at"`
	ExpiresAt    *time.Time `json:"expires_at,omitempty"`
	SupersededBy string     `json:"superseded_by,omitempty"`
}

// Active returns true if the finding has not been superseded and is not expired.
func (f *Finding) Active() bool {
	if f.SupersededBy != "" {
		return false
	}
	if f.ExpiresAt != nil && time.Now().After(*f.ExpiresAt) {
		return false
	}
	return true
}

// Envelope is the versioned wire format for scanner output.
// Scanners write this to stdout instead of raw []Finding.
type Envelope struct {
	ProtocolVersion int       `json:"protocol_version"`
	ScannerName     string    `json:"scanner_name"`
	Ability         string    `json:"ability"`
	Findings        []Finding `json:"findings"`
	Warnings        []string  `json:"warnings,omitempty"`
}

// NewEnvelope creates an envelope with the current protocol version.
func NewEnvelope(name, ability string, findings []Finding, warnings []string) Envelope {
	return Envelope{
		ProtocolVersion: 1,
		ScannerName:     name,
		Ability:         ability,
		Findings:        findings,
		Warnings:        warnings,
	}
}

// Scanner is the interface implemented by all nen scanner binaries.
// Each binary receives scope via --scope JSON flag and writes Envelope JSON to stdout.
type Scanner interface {
	Scan(ctx context.Context, scope Scope) ([]Finding, error)
}
