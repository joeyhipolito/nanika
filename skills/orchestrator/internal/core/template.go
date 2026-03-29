package core

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

// Template is a frozen plan that can be reused across missions.
// Phase objectives may contain {{param}} placeholders for substitution.
type Template struct {
	Name          string   `json:"name"`
	Task          string   `json:"task"`
	Phases        []*Phase `json:"phases"`
	ExecutionMode string   `json:"execution_mode"`
	CreatedAt     time.Time `json:"created_at"`
	SourcePlanID  string   `json:"source_plan_id,omitempty"`
}

// TemplatesDir returns the templates directory path, creating it if needed.
func TemplatesDir() (string, error) {
	base, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("config dir: %w", err)
	}
	dir := filepath.Join(base, "templates")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return "", fmt.Errorf("create templates dir: %w", err)
	}
	return dir, nil
}

// SaveTemplate writes a plan as a frozen template. Runtime state (status, output,
// error, timing) is stripped — only the structural fields are preserved.
func SaveTemplate(name string, plan *Plan) error {
	if err := validateTemplateName(name); err != nil {
		return err
	}

	dir, err := TemplatesDir()
	if err != nil {
		return err
	}

	frozen := make([]*Phase, len(plan.Phases))
	for i, p := range plan.Phases {
		frozen[i] = &Phase{
			ID:                     p.ID,
			Name:                   p.Name,
			Objective:              p.Objective,
			Persona:                p.Persona,
			ModelTier:              p.ModelTier,
			Skills:                 p.Skills,
			Constraints:            p.Constraints,
			Dependencies:           p.Dependencies,
			Expected:               p.Expected,
			PersonaSelectionMethod: p.PersonaSelectionMethod,
		}
	}

	tmpl := Template{
		Name:          name,
		Task:          plan.Task,
		Phases:        frozen,
		ExecutionMode: plan.ExecutionMode,
		CreatedAt:     time.Now(),
		SourcePlanID:  plan.ID,
	}

	data, err := json.MarshalIndent(tmpl, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal template: %w", err)
	}

	return os.WriteFile(filepath.Join(dir, name+".json"), data, 0600)
}

// LoadTemplate reads a template by name.
func LoadTemplate(name string) (*Template, error) {
	if err := validateTemplateName(name); err != nil {
		return nil, err
	}

	dir, err := TemplatesDir()
	if err != nil {
		return nil, err
	}

	data, err := os.ReadFile(filepath.Join(dir, name+".json"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("template %q not found", name)
		}
		return nil, fmt.Errorf("read template %q: %w", name, err)
	}

	var tmpl Template
	if err := json.Unmarshal(data, &tmpl); err != nil {
		return nil, fmt.Errorf("parse template %q: %w", name, err)
	}

	return &tmpl, nil
}

// ListTemplates returns all available templates.
func ListTemplates() ([]Template, error) {
	dir, err := TemplatesDir()
	if err != nil {
		return nil, err
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("read templates dir: %w", err)
	}

	var templates []Template
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".json") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(dir, e.Name()))
		if err != nil {
			continue
		}
		var tmpl Template
		if err := json.Unmarshal(data, &tmpl); err != nil {
			continue // skip malformed templates
		}
		templates = append(templates, tmpl)
	}

	return templates, nil
}

// paramPattern matches {{paramName}} placeholders.
var paramPattern = regexp.MustCompile(`\{\{(\w+)\}\}`)

// PlanFromTemplate creates a new Plan from a template, substituting {{param}}
// placeholders with provided values. Returns an error if unresolved placeholders remain.
func PlanFromTemplate(tmpl *Template, params map[string]string) (*Plan, error) {
	substitute := func(s string) string {
		return paramPattern.ReplaceAllStringFunc(s, func(match string) string {
			key := match[2 : len(match)-2]
			if val, ok := params[key]; ok {
				return val
			}
			return match
		})
	}

	task := substitute(tmpl.Task)

	phases := make([]*Phase, len(tmpl.Phases))
	for i, p := range tmpl.Phases {
		phases[i] = &Phase{
			ID:                     p.ID,
			Name:                   p.Name,
			Objective:              substitute(p.Objective),
			Persona:                p.Persona,
			ModelTier:              p.ModelTier,
			Skills:                 p.Skills,
			Constraints:            p.Constraints,
			Dependencies:           p.Dependencies,
			Expected:               substitute(p.Expected),
			PersonaSelectionMethod: p.PersonaSelectionMethod,
			Status:                 StatusPending,
		}
	}

	// Check for unresolved placeholders.
	var unresolved []string
	check := func(s, context string) {
		for _, m := range paramPattern.FindAllString(s, -1) {
			unresolved = append(unresolved, fmt.Sprintf("%s in %s", m, context))
		}
	}
	check(task, "task")
	for _, p := range phases {
		check(p.Objective, fmt.Sprintf("phase %s objective", p.ID))
		check(p.Expected, fmt.Sprintf("phase %s expected", p.ID))
	}
	if len(unresolved) > 0 {
		return nil, fmt.Errorf("unresolved template parameters: %s", strings.Join(unresolved, ", "))
	}

	return &Plan{
		ID:            fmt.Sprintf("plan_%d", time.Now().UnixNano()),
		Task:          task,
		Phases:        phases,
		ExecutionMode: tmpl.ExecutionMode,
		CreatedAt:     time.Now(),
	}, nil
}

// ParseTemplateParams extracts key=value pairs from args for template substitution.
func ParseTemplateParams(args []string) map[string]string {
	params := make(map[string]string)
	for _, arg := range args {
		if i := strings.IndexByte(arg, '='); i > 0 {
			params[arg[:i]] = arg[i+1:]
		}
	}
	return params
}

func validateTemplateName(name string) error {
	if name == "" {
		return fmt.Errorf("template name is required")
	}
	if filepath.Base(name) != name {
		return fmt.Errorf("template name must not contain path separators")
	}
	return nil
}
