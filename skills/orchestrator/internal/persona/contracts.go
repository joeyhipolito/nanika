package persona

import (
	"fmt"
	"regexp"
	"strings"
)

// OutputContract defines the expected patterns in a persona's output for a given role.
type OutputContract struct {
	// Role is the orchestrator role this contract applies to (e.g. "reviewer", "planner", "implementer").
	Role string
	// RequiredPatterns are string patterns that must appear in the output.
	// Each pattern is checked with strings.Contains.
	RequiredPatterns []string
	// RegexPatterns are compiled regex patterns that must match somewhere in the output.
	RegexPatterns []*regexp.Regexp
}

// ContractWarning describes a single contract violation (advisory, not fatal).
type ContractWarning struct {
	PersonaName string
	Role        string
	Pattern     string
	Message     string
}

func (w ContractWarning) String() string {
	return fmt.Sprintf("persona %q (role %s): missing %q — %s", w.PersonaName, w.Role, w.Pattern, w.Message)
}

// filePathPattern matches common file paths like ./foo/bar.go, src/main.ts, /absolute/path.py
var filePathPattern = regexp.MustCompile(`(?:^|[\s(])(?:\./|/)?[\w-]+(?:/[\w.-]+)+\.\w+`)

// roleContracts defines the built-in expected output patterns per role.
// These are defaults; persona-level output_requires frontmatter takes precedence.
var roleContracts = map[string]OutputContract{
	"reviewer": {
		Role:             "reviewer",
		RequiredPatterns: []string{"### Blockers", "### Warnings"},
	},
	"planner": {
		Role:            "planner",
		RequiredPatterns: []string{},
		RegexPatterns:   []*regexp.Regexp{regexp.MustCompile(`(?m)^## .+`)},
	},
	"implementer": {
		Role:          "implementer",
		RegexPatterns: []*regexp.Regexp{filePathPattern},
	},
}

// ContractForRole returns the built-in output contract for a role.
// Returns a zero-value contract (no patterns) for unknown roles.
func ContractForRole(role string) OutputContract {
	if c, ok := roleContracts[strings.ToLower(role)]; ok {
		return c
	}
	return OutputContract{Role: role}
}

// ValidateOutput checks whether output text satisfies the contract for the
// given persona and role. It checks persona-specific output_requires from
// frontmatter first, then falls back to built-in role contracts.
// Returns warnings (never errors) for missing patterns.
func ValidateOutput(personaName, role, output string) []ContractWarning {
	var warnings []ContractWarning

	// Check persona-specific output_requires from frontmatter.
	p := Get(personaName)
	if p != nil && len(p.OutputRequires) > 0 {
		for _, pattern := range p.OutputRequires {
			if !strings.Contains(output, pattern) {
				warnings = append(warnings, ContractWarning{
					PersonaName: personaName,
					Role:        role,
					Pattern:     pattern,
					Message:     "required by persona output_requires",
				})
			}
		}
		return warnings
	}

	// Fall back to built-in role contract.
	contract := ContractForRole(role)
	for _, pattern := range contract.RequiredPatterns {
		if !strings.Contains(output, pattern) {
			warnings = append(warnings, ContractWarning{
				PersonaName: personaName,
				Role:        role,
				Pattern:     pattern,
				Message:     fmt.Sprintf("expected in %s output", role),
			})
		}
	}
	for _, re := range contract.RegexPatterns {
		if !re.MatchString(output) {
			warnings = append(warnings, ContractWarning{
				PersonaName: personaName,
				Role:        role,
				Pattern:     re.String(),
				Message:     fmt.Sprintf("regex pattern expected in %s output", role),
			})
		}
	}

	return warnings
}
