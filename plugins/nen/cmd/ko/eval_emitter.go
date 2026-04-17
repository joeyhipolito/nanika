package main

import (
	"context"
	"crypto/sha256"
	"fmt"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
	"github.com/joeyhipolito/nen/ko"
)

const (
	maxEvidenceItems = 10
	koAbility        = "ko-eval"
	koCategory       = "eval-failure"
	koScopeKind      = "eval-config"
	koEvidenceKind   = "ko-test-failure"
)

// SuiteResult is an aggregated summary of one eval suite (config-level).
type SuiteResult struct {
	// ConfigPath is the absolute path to the eval config file that produced this suite.
	ConfigPath string
	// Name is the suite identifier (currently equals ConfigPath; reserved for
	// future per-suite granularity within a single config).
	Name   string
	Total  int
	Passed int
	Failed int
	// FailedTests holds descriptions + first failure messages for evidence.
	FailedTests []failedTest
}

type failedTest struct {
	Description string
	Message     string
	Source      string
}

// thresholdSeverity maps a pass rate (0–100) to a finding severity.
// Returns "" for pass rates >= 80 (not emitted).
func thresholdSeverity(passRate float64) scan.Severity {
	switch {
	case passRate < 60:
		return scan.SeverityHigh
	case passRate < 80:
		return scan.SeverityMedium
	default:
		return ""
	}
}

// deterministicKoFindingID produces a stable finding ID for a given config+suite.
func deterministicKoFindingID(configPath, suite string) string {
	h := sha256.Sum256([]byte(koAbility + ":" + koCategory + ":" + koScopeKind + ":" + configPath + ":" + suite))
	return fmt.Sprintf("ko-%x", h[:4])
}

// aggregateBySuite groups test results into one SuiteResult per config.
// Ko configs don't currently expose a suite-per-test field, so all tests
// in a single evaluate run form one suite keyed by config path.
func aggregateBySuite(configPath string, results []ko.TestResult) []SuiteResult {
	suite := SuiteResult{ConfigPath: configPath, Name: configPath}
	for _, r := range results {
		suite.Total++
		if r.Passed {
			suite.Passed++
		} else {
			suite.Failed++
			msg := r.Error
			if msg == "" {
				for _, a := range r.Assertions {
					if !a.Passed {
						msg = a.Type + ": " + a.Message
						break
					}
				}
			}
			suite.FailedTests = append(suite.FailedTests, failedTest{
				Description: r.Description,
				Message:     msg,
				Source:      configPath + "#" + r.Description,
			})
		}
	}
	return []SuiteResult{suite}
}

// emitFindings persists one scan.Finding per suite whose pass rate falls
// below the threshold. Returns the findings that were emitted.
func emitFindings(ctx context.Context, suites []SuiteResult, now time.Time) ([]scan.Finding, error) {
	var findings []scan.Finding

	for _, s := range suites {
		var passRate float64
		if s.Total > 0 {
			passRate = float64(s.Passed) / float64(s.Total) * 100
		}
		// Crashed suite (zero tests) → 0% pass rate.

		sev := thresholdSeverity(passRate)
		if sev == "" {
			// Suite is above threshold — supersede any prior finding for this scope
			// so stale alerts don't linger after recovery.
			if err := scan.SupersedeActiveFindingsForScope(
				ctx,
				koAbility, koCategory, koScopeKind, s.Name,
				deterministicKoFindingID(s.ConfigPath, s.Name),
			); err != nil {
				return nil, fmt.Errorf("supersede recovered ko finding: %w", err)
			}
			continue
		}

		var evidence []scan.Evidence
		for i, ft := range s.FailedTests {
			if i >= maxEvidenceItems {
				evidence = append(evidence, scan.Evidence{
					Kind:       koEvidenceKind,
					Raw:        fmt.Sprintf("… and %d more failing test(s)", len(s.FailedTests)-maxEvidenceItems),
					Source:     s.Name,
					CapturedAt: now,
				})
				break
			}
			evidence = append(evidence, scan.Evidence{
				Kind:       koEvidenceKind,
				Raw:        ft.Description + " — " + ft.Message,
				Source:     ft.Source,
				CapturedAt: now,
			})
		}

		findings = append(findings, scan.Finding{
			ID:          deterministicKoFindingID(s.ConfigPath, s.Name),
			Ability:     koAbility,
			Category:    koCategory,
			Severity:    sev,
			Title:       fmt.Sprintf("%s eval: %d/%d passing (%.0f%%)", s.Name, s.Passed, s.Total, passRate),
			Description: "Ko eval suite below threshold. See evidence for failing tests.",
			Scope:       scan.Scope{Kind: koScopeKind, Value: s.Name},
			Evidence:    evidence,
			Source:      "ko evaluate",
			FoundAt:     now,
		})
	}

	if len(findings) == 0 {
		return nil, nil
	}

	if err := scan.PersistFindings(ctx, findings); err != nil {
		return nil, fmt.Errorf("persist ko findings: %w", err)
	}

	return findings, nil
}
