package core

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestSaveAndLoadTemplate(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	plan := &Plan{
		ID:            "plan_123",
		Task:          "build a REST API",
		ExecutionMode: "parallel",
		CreatedAt:     time.Now(),
		Phases: []*Phase{
			{
				ID:        "phase-1",
				Name:      "design",
				Objective: "design the API",
				Persona:   "architect",
				ModelTier: "think",
				Status:    StatusCompleted,
				Output:    "should be stripped",
				Error:     "should be stripped",
			},
			{
				ID:           "phase-2",
				Name:         "implement",
				Objective:    "implement endpoints",
				Persona:      "senior-backend-engineer",
				ModelTier:    "work",
				Dependencies: []string{"phase-1"},
				Status:       StatusFailed,
				Error:        "should be stripped",
				Retries:      2,
			},
		},
	}

	if err := SaveTemplate("api-build", plan); err != nil {
		t.Fatalf("SaveTemplate: %v", err)
	}

	// Verify file exists
	tmplPath := filepath.Join(dir, "templates", "api-build.json")
	if _, err := os.Stat(tmplPath); err != nil {
		t.Fatalf("template file not created: %v", err)
	}

	// Load and verify
	tmpl, err := LoadTemplate("api-build")
	if err != nil {
		t.Fatalf("LoadTemplate: %v", err)
	}

	if tmpl.Name != "api-build" {
		t.Errorf("name = %q, want %q", tmpl.Name, "api-build")
	}
	if tmpl.SourcePlanID != "plan_123" {
		t.Errorf("source_plan_id = %q, want %q", tmpl.SourcePlanID, "plan_123")
	}
	if len(tmpl.Phases) != 2 {
		t.Fatalf("phases = %d, want 2", len(tmpl.Phases))
	}

	// Runtime state must be stripped
	for _, p := range tmpl.Phases {
		if p.Status != "" {
			t.Errorf("phase %s status = %q, want empty (stripped)", p.ID, p.Status)
		}
		if p.Output != "" {
			t.Errorf("phase %s output not stripped", p.ID)
		}
		if p.Error != "" {
			t.Errorf("phase %s error not stripped", p.ID)
		}
		if p.Retries != 0 {
			t.Errorf("phase %s retries = %d, want 0 (stripped)", p.ID, p.Retries)
		}
	}

	// Structural fields preserved
	if tmpl.Phases[1].Dependencies[0] != "phase-1" {
		t.Errorf("phase-2 dependency not preserved")
	}
}

func TestLoadTemplateNotFound(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	_, err := LoadTemplate("nonexistent")
	if err == nil {
		t.Fatal("expected error for missing template")
	}
}

func TestListTemplates(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	// Empty list
	templates, err := ListTemplates()
	if err != nil {
		t.Fatalf("ListTemplates (empty): %v", err)
	}
	if len(templates) != 0 {
		t.Fatalf("expected 0 templates, got %d", len(templates))
	}

	// Save two templates
	plan := &Plan{
		ID:            "plan_1",
		Task:          "task one",
		ExecutionMode: "sequential",
		Phases:        []*Phase{{ID: "phase-1", Name: "do", Objective: "do it"}},
	}
	if err := SaveTemplate("alpha", plan); err != nil {
		t.Fatalf("save alpha: %v", err)
	}
	plan.Task = "task two"
	if err := SaveTemplate("beta", plan); err != nil {
		t.Fatalf("save beta: %v", err)
	}

	templates, err = ListTemplates()
	if err != nil {
		t.Fatalf("ListTemplates: %v", err)
	}
	if len(templates) != 2 {
		t.Fatalf("expected 2 templates, got %d", len(templates))
	}
}

func TestPlanFromTemplate(t *testing.T) {
	tests := []struct {
		name       string
		tmpl       *Template
		params     map[string]string
		wantTask   string
		wantObj    string // phase-1 objective
		wantErr    bool
		errContain string
	}{
		{
			name: "no params",
			tmpl: &Template{
				Name:          "simple",
				Task:          "deploy the app",
				ExecutionMode: "sequential",
				Phases: []*Phase{
					{ID: "phase-1", Name: "deploy", Objective: "deploy the app", Persona: "devops"},
				},
			},
			params:  nil,
			wantTask: "deploy the app",
			wantObj:  "deploy the app",
		},
		{
			name: "single substitution",
			tmpl: &Template{
				Name:          "deploy",
				Task:          "deploy {{service}}",
				ExecutionMode: "sequential",
				Phases: []*Phase{
					{ID: "phase-1", Name: "deploy", Objective: "deploy {{service}} to prod", Persona: "devops"},
				},
			},
			params:  map[string]string{"service": "users-api"},
			wantTask: "deploy users-api",
			wantObj:  "deploy users-api to prod",
		},
		{
			name: "multiple params",
			tmpl: &Template{
				Name: "migrate",
				Task: "migrate {{db}} for {{service}}",
				Phases: []*Phase{
					{ID: "phase-1", Name: "migrate", Objective: "run {{db}} migrations for {{service}}", Persona: "backend"},
				},
			},
			params:  map[string]string{"db": "postgres", "service": "auth"},
			wantTask: "migrate postgres for auth",
			wantObj:  "run postgres migrations for auth",
		},
		{
			name: "unresolved param errors",
			tmpl: &Template{
				Name: "broken",
				Task: "deploy {{service}}",
				Phases: []*Phase{
					{ID: "phase-1", Name: "deploy", Objective: "deploy {{service}}", Persona: "devops"},
				},
			},
			params:     map[string]string{},
			wantErr:    true,
			errContain: "unresolved template parameters",
		},
		{
			name: "expected field substitution",
			tmpl: &Template{
				Name: "with-expected",
				Task: "test {{module}}",
				Phases: []*Phase{
					{
						ID:        "phase-1",
						Name:      "test",
						Objective: "test {{module}}",
						Expected:  "all {{module}} tests pass",
						Persona:   "qa",
					},
				},
			},
			params:  map[string]string{"module": "auth"},
			wantTask: "test auth",
			wantObj:  "test auth",
		},
		{
			name: "phases get StatusPending",
			tmpl: &Template{
				Name: "fresh",
				Task: "do stuff",
				Phases: []*Phase{
					{ID: "phase-1", Name: "do", Objective: "do stuff", Persona: "dev"},
				},
			},
			params:  nil,
			wantTask: "do stuff",
			wantObj:  "do stuff",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			plan, err := PlanFromTemplate(tt.tmpl, tt.params)

			if tt.wantErr {
				if err == nil {
					t.Fatal("expected error, got nil")
				}
				if tt.errContain != "" && !strings.Contains(err.Error(), tt.errContain) {
					t.Errorf("error = %q, want containing %q", err.Error(), tt.errContain)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}

			if plan.Task != tt.wantTask {
				t.Errorf("task = %q, want %q", plan.Task, tt.wantTask)
			}
			if plan.Phases[0].Objective != tt.wantObj {
				t.Errorf("objective = %q, want %q", plan.Phases[0].Objective, tt.wantObj)
			}

			// Every phase must be pending
			for _, p := range plan.Phases {
				if p.Status != StatusPending {
					t.Errorf("phase %s status = %q, want %q", p.ID, p.Status, StatusPending)
				}
			}

			// Plan gets a fresh ID
			if plan.ID == tt.tmpl.SourcePlanID {
				t.Error("plan ID should be fresh, not reuse source plan ID")
			}
		})
	}
}

func TestValidateTemplateName(t *testing.T) {
	tests := []struct {
		name    string
		wantErr bool
	}{
		{"deploy-api", false},
		{"my_template", false},
		{"simple", false},
		{"", true},
		{"../escape", true},
		{"sub/dir", true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validateTemplateName(tt.name)
			if (err != nil) != tt.wantErr {
				t.Errorf("validateTemplateName(%q) error = %v, wantErr = %v", tt.name, err, tt.wantErr)
			}
		})
	}
}

func TestSaveTemplateOverwrite(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	plan1 := &Plan{
		ID:   "plan_1",
		Task: "original task",
		Phases: []*Phase{
			{ID: "phase-1", Name: "v1", Objective: "original"},
		},
	}
	plan2 := &Plan{
		ID:   "plan_2",
		Task: "updated task",
		Phases: []*Phase{
			{ID: "phase-1", Name: "v2", Objective: "updated"},
		},
	}

	if err := SaveTemplate("overwrite-me", plan1); err != nil {
		t.Fatalf("first save: %v", err)
	}
	if err := SaveTemplate("overwrite-me", plan2); err != nil {
		t.Fatalf("second save: %v", err)
	}

	tmpl, err := LoadTemplate("overwrite-me")
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if tmpl.Task != "updated task" {
		t.Errorf("task = %q, want %q (should be overwritten)", tmpl.Task, "updated task")
	}
	if tmpl.Phases[0].Name != "v2" {
		t.Errorf("phase name = %q, want %q", tmpl.Phases[0].Name, "v2")
	}
}

func TestParseTemplateParams(t *testing.T) {
	tests := []struct {
		name string
		args []string
		want map[string]string
	}{
		{"empty", nil, map[string]string{}},
		{"single", []string{"service=users"}, map[string]string{"service": "users"}},
		{"multiple", []string{"db=postgres", "service=auth"}, map[string]string{"db": "postgres", "service": "auth"}},
		{"value with equals", []string{"url=http://host:8080/path?a=1"}, map[string]string{"url": "http://host:8080/path?a=1"}},
		{"non-param args ignored", []string{"hello", "world"}, map[string]string{}},
		{"mixed", []string{"deploy", "service=api"}, map[string]string{"service": "api"}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ParseTemplateParams(tt.args)
			if len(got) != len(tt.want) {
				t.Fatalf("len = %d, want %d", len(got), len(tt.want))
			}
			for k, v := range tt.want {
				if got[k] != v {
					t.Errorf("params[%q] = %q, want %q", k, got[k], v)
				}
			}
		})
	}
}

