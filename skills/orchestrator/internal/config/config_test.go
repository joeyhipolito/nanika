package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestDir_OrchestratorConfigDir(t *testing.T) {
	t.Setenv(EnvVar, "/tmp/orch-override")
	t.Setenv(EnvVarViaHome, "/tmp/via-home")

	got, err := Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	if got != "/tmp/orch-override" {
		t.Errorf("Dir() = %q, want %q", got, "/tmp/orch-override")
	}
}

func TestDir_ViaHome(t *testing.T) {
	t.Setenv(EnvVar, "")
	t.Setenv(EnvVarAllukaHome, "")
	t.Setenv(EnvVarViaHome, "/tmp/via-home")

	got, err := Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	want := filepath.Join("/tmp/via-home", "orchestrator")
	if got != want {
		t.Errorf("Dir() = %q, want %q", got, want)
	}
}

func TestDir_Default(t *testing.T) {
	t.Setenv(EnvVar, "")
	t.Setenv(EnvVarAllukaHome, "")
	t.Setenv(EnvVarViaHome, "")

	got, err := Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	home, _ := os.UserHomeDir()
	want := filepath.Join(home, DirName)
	if got != want {
		t.Errorf("Dir() = %q, want %q", got, want)
	}
}

func TestDir_OrchestratorOverridesViaHome(t *testing.T) {
	t.Setenv(EnvVar, "/custom/path")
	t.Setenv(EnvVarViaHome, "/should/not/use")

	got, err := Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	if got != "/custom/path" {
		t.Errorf("ORCHESTRATOR_CONFIG_DIR should take priority, got %q", got)
	}
}
