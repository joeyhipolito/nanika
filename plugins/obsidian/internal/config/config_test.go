package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestNewStoreWithEnv_defaultDir(t *testing.T) {
	t.Setenv(ConfigDirEnv, "")
	s := NewStoreWithEnv(ConfigDirEnv)

	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip("cannot determine home directory")
	}

	got, err := s.Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	want := filepath.Join(home, ConfigDir)
	if got != want {
		t.Errorf("Dir() = %q, want %q", got, want)
	}
}

func TestNewStoreWithEnv_envOverride(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv(ConfigDirEnv, tmp)
	s := NewStoreWithEnv(ConfigDirEnv)

	got, err := s.Dir()
	if err != nil {
		t.Fatalf("Dir() error: %v", err)
	}
	if got != tmp {
		t.Errorf("Dir() = %q, want %q", got, tmp)
	}

	gotPath, err := s.Path()
	if err != nil {
		t.Fatalf("Path() error: %v", err)
	}
	wantPath := filepath.Join(tmp, ConfigFile)
	if gotPath != wantPath {
		t.Errorf("Path() = %q, want %q", gotPath, wantPath)
	}
}

func TestStore_ExistsAndLoadMissing(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv(ConfigDirEnv, tmp)
	s := NewStoreWithEnv(ConfigDirEnv)

	if s.Exists() {
		t.Fatal("Exists() = true before file created")
	}

	cfg, err := s.Load()
	if err != nil {
		t.Fatalf("Load() on missing file error: %v", err)
	}
	if cfg.GeminiAPIKey != "" || cfg.VaultPath != "" {
		t.Errorf("Load() on missing file returned non-empty config: %+v", cfg)
	}
}

func TestStore_SaveAndLoad(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv(ConfigDirEnv, tmp)
	s := NewStoreWithEnv(ConfigDirEnv)

	want := &Config{
		GeminiAPIKey: "testkey",
		VaultPath:    "/tmp/vault",
		WebsitePath:  "/tmp/site",
	}
	if err := s.Save(want); err != nil {
		t.Fatalf("Save() error: %v", err)
	}

	if !s.Exists() {
		t.Fatal("Exists() = false after Save()")
	}

	got, err := s.Load()
	if err != nil {
		t.Fatalf("Load() error: %v", err)
	}
	if got.GeminiAPIKey != want.GeminiAPIKey {
		t.Errorf("GeminiAPIKey = %q, want %q", got.GeminiAPIKey, want.GeminiAPIKey)
	}
	if got.VaultPath != want.VaultPath {
		t.Errorf("VaultPath = %q, want %q", got.VaultPath, want.VaultPath)
	}
	if got.WebsitePath != want.WebsitePath {
		t.Errorf("WebsitePath = %q, want %q", got.WebsitePath, want.WebsitePath)
	}
}

func TestStore_Permissions(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv(ConfigDirEnv, tmp)
	s := NewStoreWithEnv(ConfigDirEnv)

	if err := s.Save(&Config{GeminiAPIKey: "k", VaultPath: "/v"}); err != nil {
		t.Fatalf("Save() error: %v", err)
	}

	perms, err := s.Permissions()
	if err != nil {
		t.Fatalf("Permissions() error: %v", err)
	}
	if perms != 0600 {
		t.Errorf("Permissions() = %o, want 600", perms)
	}
}

func TestPackageLevelFunctions_respectEnvVar(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv(ConfigDirEnv, tmp)

	// package-level Path() and Dir() should reflect env var
	if Dir() != tmp {
		t.Errorf("Dir() = %q, want %q", Dir(), tmp)
	}
	if Path() != filepath.Join(tmp, ConfigFile) {
		t.Errorf("Path() = %q, want %q", Path(), filepath.Join(tmp, ConfigFile))
	}

	if err := Save(&Config{GeminiAPIKey: "pk", VaultPath: "/pv"}); err != nil {
		t.Fatalf("Save() error: %v", err)
	}
	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error: %v", err)
	}
	if cfg.GeminiAPIKey != "pk" {
		t.Errorf("Load() GeminiAPIKey = %q, want %q", cfg.GeminiAPIKey, "pk")
	}
	if !Exists() {
		t.Error("Exists() = false after Save()")
	}
}
