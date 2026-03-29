package scan

import (
	"os"
	"path/filepath"
	"testing"
)

func TestDir_OrchestratorConfigDir(t *testing.T) {
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", "/tmp/orch-override")
	t.Setenv("ALLUKA_HOME", "/tmp/alluka-home")
	t.Setenv("VIA_HOME", "/tmp/via-home")

	got, err := Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	if got != "/tmp/orch-override" {
		t.Fatalf("Dir() = %q, want %q", got, "/tmp/orch-override")
	}
}

func TestDir_ViaHome(t *testing.T) {
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", "")
	t.Setenv("ALLUKA_HOME", "")
	t.Setenv("VIA_HOME", "/tmp/via-home")

	got, err := Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	want := filepath.Join("/tmp/via-home", "orchestrator")
	if got != want {
		t.Fatalf("Dir() = %q, want %q", got, want)
	}
}

func TestDir_DefaultPrefersAllukaWhenPresent(t *testing.T) {
	home := t.TempDir()
	alluka := filepath.Join(home, ".alluka")
	if err := os.MkdirAll(alluka, 0755); err != nil {
		t.Fatal(err)
	}

	t.Setenv("HOME", home)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", "")
	t.Setenv("ALLUKA_HOME", "")
	t.Setenv("VIA_HOME", "")

	got, err := Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	if got != alluka {
		t.Fatalf("Dir() = %q, want %q", got, alluka)
	}
}
