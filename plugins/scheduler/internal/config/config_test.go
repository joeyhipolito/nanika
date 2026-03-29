package config

import (
	"os"
	"path/filepath"
	"testing"
)

// TestNewStoreWithEnv verifies that NewStoreWithEnv reads SCHEDULER_CONFIG_DIR.
func TestNewStoreWithEnv(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv("SCHEDULER_CONFIG_DIR", tmp)

	s := NewStoreWithEnv()
	if s.Dir() != tmp {
		t.Errorf("Dir() = %q, want %q", s.Dir(), tmp)
	}
	if s.Path() != filepath.Join(tmp, FileName) {
		t.Errorf("Path() = %q, want %q", s.Path(), filepath.Join(tmp, FileName))
	}
}

// TestNewStoreWithEnvFallback verifies that NewStoreWithEnv falls back to ~/.scheduler
// when SCHEDULER_CONFIG_DIR is unset.
func TestNewStoreWithEnvFallback(t *testing.T) {
	os.Unsetenv("SCHEDULER_CONFIG_DIR")
	t.Cleanup(func() { os.Unsetenv("SCHEDULER_CONFIG_DIR") })

	s := NewStoreWithEnv()
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip("no home dir, skipping fallback test")
	}
	want := filepath.Join(home, DirName)
	if s.Dir() != want {
		t.Errorf("Dir() = %q, want %q", s.Dir(), want)
	}
}

// TestStoreExistsAndLoad verifies round-trip save/load using a temp dir.
func TestStoreExistsAndLoad(t *testing.T) {
	s := newStoreWithDir(t.TempDir())

	if s.Exists() {
		t.Fatal("expected config file to not exist before Save")
	}

	cfg := &Config{
		DBPath:         filepath.Join(s.Dir(), "scheduler.db"),
		LogLevel:       "debug",
		Shell:          "/bin/bash",
		MaxConcurrent:  8,
		DashboardToken: "tok123",
	}
	if err := s.Save(cfg); err != nil {
		t.Fatalf("Save: %v", err)
	}

	if !s.Exists() {
		t.Fatal("expected config file to exist after Save")
	}

	got, err := s.Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}

	if got.DBPath != cfg.DBPath {
		t.Errorf("DBPath = %q, want %q", got.DBPath, cfg.DBPath)
	}
	if got.LogLevel != cfg.LogLevel {
		t.Errorf("LogLevel = %q, want %q", got.LogLevel, cfg.LogLevel)
	}
	if got.Shell != cfg.Shell {
		t.Errorf("Shell = %q, want %q", got.Shell, cfg.Shell)
	}
	if got.MaxConcurrent != cfg.MaxConcurrent {
		t.Errorf("MaxConcurrent = %d, want %d", got.MaxConcurrent, cfg.MaxConcurrent)
	}
	if got.DashboardToken != cfg.DashboardToken {
		t.Errorf("DashboardToken = %q, want %q", got.DashboardToken, cfg.DashboardToken)
	}
}

// TestStoreLoadDefaults verifies that Load returns defaults when no file exists.
func TestStoreLoadDefaults(t *testing.T) {
	s := newStoreWithDir(t.TempDir())

	cfg, err := s.Load()
	if err != nil {
		t.Fatalf("Load on missing file: %v", err)
	}
	if cfg.LogLevel != "info" {
		t.Errorf("LogLevel = %q, want %q", cfg.LogLevel, "info")
	}
	if cfg.Shell != "/bin/sh" {
		t.Errorf("Shell = %q, want %q", cfg.Shell, "/bin/sh")
	}
	if cfg.MaxConcurrent != 4 {
		t.Errorf("MaxConcurrent = %d, want 4", cfg.MaxConcurrent)
	}
}

// TestStoreLoadMalformed verifies that Load rejects invalid key=value lines.
func TestStoreLoadMalformed(t *testing.T) {
	s := newStoreWithDir(t.TempDir())
	if err := os.WriteFile(s.Path(), []byte("not_a_valid_line\n"), 0600); err != nil {
		t.Fatal(err)
	}
	_, err := s.Load()
	if err == nil {
		t.Fatal("expected error for malformed config, got nil")
	}
}

// TestStorePermissions verifies that Save writes the config file with 0600 permissions.
func TestStorePermissions(t *testing.T) {
	s := newStoreWithDir(t.TempDir())
	cfg := Default()
	cfg.DBPath = filepath.Join(s.Dir(), "scheduler.db")

	if err := s.Save(cfg); err != nil {
		t.Fatalf("Save: %v", err)
	}
	perm, err := s.Permissions()
	if err != nil {
		t.Fatalf("Permissions: %v", err)
	}
	if perm != 0600 {
		t.Errorf("permissions = %04o, want 0600", perm)
	}
}

// TestStoreEnsureDir verifies that EnsureDir creates the directory.
func TestStoreEnsureDir(t *testing.T) {
	tmp := t.TempDir()
	s := newStoreWithDir(filepath.Join(tmp, "nested", "dir"))

	if err := s.EnsureDir(); err != nil {
		t.Fatalf("EnsureDir: %v", err)
	}
	if _, err := os.Stat(s.Dir()); err != nil {
		t.Errorf("directory not created: %v", err)
	}
}
