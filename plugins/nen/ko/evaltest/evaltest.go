// Package evaltest provides utilities for testing decomposer LLM outputs.
// It calls the claude CLI, parses PHASE lines from the output, and offers
// assertion helpers that validate phase plans against common invariants.
package evaltest

import (
	"context"
	"fmt"
	"strings"

	"github.com/joeyhipolito/nen/ko"
)

// Phase represents a single parsed PHASE line from decomposer output.
type Phase struct {
	Name      string
	Objective string
	Persona   string
	Skills    []string
	Depends   []string
}

// Eval calls the claude CLI with the given model and prompt, returning stdout.
func Eval(ctx context.Context, model, prompt string) (string, error) {
	return ko.CallLLM(ctx, model, prompt)
}

// ParsePhases extracts Phase objects from LLM output containing PHASE lines.
// Lines not matching the PHASE: format are silently skipped.
func ParsePhases(output string) []Phase {
	var phases []Phase
	for _, line := range strings.Split(output, "\n") {
		line = strings.TrimSpace(line)
		if !strings.HasPrefix(line, "PHASE:") {
			continue
		}
		p := parsePhaseLine(line)
		if p.Name != "" {
			phases = append(phases, p)
		}
	}
	return phases
}

// parsePhaseLine parses a single pipe-delimited PHASE line.
func parsePhaseLine(line string) Phase {
	parts := strings.Split(line, "|")
	var p Phase
	for _, part := range parts {
		part = strings.TrimSpace(part)
		if v, ok := cutField(part, "PHASE:"); ok {
			p.Name = v
		} else if v, ok := cutField(part, "OBJECTIVE:"); ok {
			p.Objective = v
		} else if v, ok := cutField(part, "PERSONA:"); ok {
			p.Persona = v
		} else if v, ok := cutField(part, "SKILLS:"); ok {
			p.Skills = splitComma(v)
		} else if v, ok := cutField(part, "DEPENDS:"); ok {
			p.Depends = splitComma(v)
		}
	}
	return p
}

// cutField extracts the trimmed value after prefix if the string starts with prefix.
func cutField(s, prefix string) (string, bool) {
	after, ok := strings.CutPrefix(s, prefix)
	if !ok {
		return "", false
	}
	return strings.TrimSpace(after), true
}

// splitComma splits a comma-separated string into trimmed, non-empty tokens.
func splitComma(s string) []string {
	s = strings.TrimSpace(s)
	if s == "" {
		return nil
	}
	parts := strings.Split(s, ",")
	result := make([]string, 0, len(parts))
	for _, p := range parts {
		p = strings.TrimSpace(p)
		if p != "" {
			result = append(result, p)
		}
	}
	return result
}

// AssertPersonasInSet passes when every phase's Persona appears in validPersonas.
func AssertPersonasInSet(phases []Phase, validPersonas []string) (bool, string) {
	valid := make(map[string]bool, len(validPersonas))
	for _, p := range validPersonas {
		valid[p] = true
	}
	for _, ph := range phases {
		if !valid[ph.Persona] {
			return false, fmt.Sprintf("phase %q uses persona %q which is not in the allowed set", ph.Name, ph.Persona)
		}
	}
	return true, ""
}

// AssertDepsValid passes when every phase name referenced in any Depends list
// corresponds to a phase that exists in the plan.
func AssertDepsValid(phases []Phase) (bool, string) {
	names := make(map[string]bool, len(phases))
	for _, ph := range phases {
		names[ph.Name] = true
	}
	for _, ph := range phases {
		for _, dep := range ph.Depends {
			if !names[dep] {
				return false, fmt.Sprintf("phase %q depends on %q which does not exist in the plan", ph.Name, dep)
			}
		}
	}
	return true, ""
}

// AssertPhaseCountRange passes when len(phases) is between min and max (inclusive).
func AssertPhaseCountRange(phases []Phase, min, max int) (bool, string) {
	n := len(phases)
	if n < min || n > max {
		return false, fmt.Sprintf("phase count %d is not in range [%d, %d]", n, min, max)
	}
	return true, ""
}

// AssertContainsPersona passes when at least one phase uses the given persona.
func AssertContainsPersona(phases []Phase, persona string) (bool, string) {
	for _, ph := range phases {
		if ph.Persona == persona {
			return true, ""
		}
	}
	return false, fmt.Sprintf("no phase uses persona %q", persona)
}

// AssertFieldNotContains passes when the named field of every phase does not
// contain substr. Valid field names: "name", "objective", "persona".
func AssertFieldNotContains(phases []Phase, field, substr string) (bool, string) {
	for _, ph := range phases {
		var val string
		switch field {
		case "name":
			val = ph.Name
		case "objective":
			val = ph.Objective
		case "persona":
			val = ph.Persona
		default:
			return false, fmt.Sprintf("unknown field %q; valid fields: name, objective, persona", field)
		}
		if strings.Contains(val, substr) {
			return false, fmt.Sprintf("phase %q field %q contains forbidden substring %q", ph.Name, field, substr)
		}
	}
	return true, ""
}

// AssertNoPipeInObjective passes when no phase's Objective contains a literal
// pipe character. Pipes in objectives corrupt the PHASE line format.
func AssertNoPipeInObjective(phases []Phase) (bool, string) {
	for _, ph := range phases {
		if strings.Contains(ph.Objective, "|") {
			return false, fmt.Sprintf("phase %q objective contains a pipe character", ph.Name)
		}
	}
	return true, ""
}
