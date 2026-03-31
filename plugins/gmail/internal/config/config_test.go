package config

import (
	"os"
	"path/filepath"
	"testing"
)

// withTempHome sets HOME to a temp directory for the duration of the test.
// Returns the temp directory path.
func withTempHome(t *testing.T) string {
	t.Helper()
	tmp := t.TempDir()
	t.Setenv("HOME", tmp)
	return tmp
}

// TestLoad_MissingFile verifies that Load returns an empty Config (not an error)
// when no config file exists.
func TestLoad_MissingFile(t *testing.T) {
	withTempHome(t)

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() on missing file returned error: %v", err)
	}
	if cfg == nil {
		t.Fatal("Load() returned nil config")
	}
	if cfg.ClientID != "" || cfg.ClientSecret != "" {
		t.Errorf("Load() expected empty config, got ClientID=%q ClientSecret=%q", cfg.ClientID, cfg.ClientSecret)
	}
}

// TestLoad_ValidFile verifies that all fields are parsed correctly from a config file.
func TestLoad_ValidFile(t *testing.T) {
	withTempHome(t)

	content := "client_id=test-id-123\nclient_secret=test-secret-456\n"
	if err := os.MkdirAll(Dir(), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(Path(), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() returned error: %v", err)
	}
	if cfg.ClientID != "test-id-123" {
		t.Errorf("ClientID = %q, want %q", cfg.ClientID, "test-id-123")
	}
	if cfg.ClientSecret != "test-secret-456" {
		t.Errorf("ClientSecret = %q, want %q", cfg.ClientSecret, "test-secret-456")
	}
}

// TestLoad_EnvVarFallback verifies that GMAIL_CLIENT_ID/SECRET are applied when
// no config file exists.
func TestLoad_EnvVarFallback(t *testing.T) {
	withTempHome(t)
	t.Setenv(EnvClientID, "env-id")
	t.Setenv(EnvClientSecret, "env-secret")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() returned error: %v", err)
	}
	if cfg.ClientID != "env-id" {
		t.Errorf("ClientID = %q, want %q via env var", cfg.ClientID, "env-id")
	}
	if cfg.ClientSecret != "env-secret" {
		t.Errorf("ClientSecret = %q, want %q via env var", cfg.ClientSecret, "env-secret")
	}
}

// TestLoad_FileWinsOverEnvVar verifies that config file values take priority
// over environment variables.
func TestLoad_FileWinsOverEnvVar(t *testing.T) {
	withTempHome(t)
	t.Setenv(EnvClientID, "env-id")
	t.Setenv(EnvClientSecret, "env-secret")

	content := "client_id=file-id\nclient_secret=file-secret\n"
	if err := os.MkdirAll(Dir(), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(Path(), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() returned error: %v", err)
	}
	if cfg.ClientID != "file-id" {
		t.Errorf("ClientID = %q, want file value %q (not env var)", cfg.ClientID, "file-id")
	}
}

// TestSaveLoad_Roundtrip verifies that Save followed by Load preserves all fields.
func TestSaveLoad_Roundtrip(t *testing.T) {
	withTempHome(t)

	original := &Config{
		ClientID:     "roundtrip-id",
		ClientSecret: "roundtrip-secret",
	}

	if err := Save(original); err != nil {
		t.Fatalf("Save() returned error: %v", err)
	}

	loaded, err := Load()
	if err != nil {
		t.Fatalf("Load() returned error: %v", err)
	}
	if loaded.ClientID != original.ClientID {
		t.Errorf("ClientID = %q, want %q", loaded.ClientID, original.ClientID)
	}
	if loaded.ClientSecret != original.ClientSecret {
		t.Errorf("ClientSecret = %q, want %q", loaded.ClientSecret, original.ClientSecret)
	}
}

// TestSave_Permissions verifies the config file is created with 0600 permissions.
func TestSave_Permissions(t *testing.T) {
	withTempHome(t)

	cfg := &Config{ClientID: "id", ClientSecret: "secret"}
	if err := Save(cfg); err != nil {
		t.Fatalf("Save() returned error: %v", err)
	}

	info, err := os.Stat(Path())
	if err != nil {
		t.Fatalf("os.Stat() returned error: %v", err)
	}
	perm := info.Mode().Perm()
	if perm != 0600 {
		t.Errorf("config file permissions = %04o, want 0600", perm)
	}
}

// TestAddAccount_RejectsDuplicate verifies that adding an account with a
// duplicate alias returns an error.
func TestAddAccount_RejectsDuplicate(t *testing.T) {
	withTempHome(t)

	if err := AddAccount("work", "work@example.com"); err != nil {
		t.Fatalf("AddAccount() first call failed: %v", err)
	}
	if err := AddAccount("work", "other@example.com"); err == nil {
		t.Error("AddAccount() should return error for duplicate alias, got nil")
	}
}

// TestRemoveAccount_CleansUpTokenFile verifies that removing an account also
// deletes the associated token file when it exists.
func TestRemoveAccount_CleansUpTokenFile(t *testing.T) {
	withTempHome(t)

	// Create the account.
	if err := AddAccount("temp", "temp@example.com"); err != nil {
		t.Fatalf("AddAccount() failed: %v", err)
	}

	// Create a fake token file.
	tokenPath := TokenPath("temp")
	if err := os.MkdirAll(filepath.Dir(tokenPath), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(tokenPath, []byte(`{}`), 0600); err != nil {
		t.Fatal(err)
	}

	if err := RemoveAccount("temp"); err != nil {
		t.Fatalf("RemoveAccount() failed: %v", err)
	}

	if _, err := os.Stat(tokenPath); !os.IsNotExist(err) {
		t.Errorf("token file still exists after RemoveAccount()")
	}
}

// TestLoadAccounts_EmptyFile verifies that LoadAccounts returns nil (not an error)
// when the accounts file is missing.
func TestLoadAccounts_EmptyFile(t *testing.T) {
	withTempHome(t)

	accounts, err := LoadAccounts()
	if err != nil {
		t.Fatalf("LoadAccounts() on missing file returned error: %v", err)
	}
	if len(accounts) != 0 {
		t.Errorf("LoadAccounts() expected empty slice, got %d accounts", len(accounts))
	}
}

// TestGetAccount_NotFound verifies that GetAccount returns an error for an unknown alias.
func TestGetAccount_NotFound(t *testing.T) {
	withTempHome(t)

	_, err := GetAccount("nonexistent")
	if err == nil {
		t.Error("GetAccount() should return error for unknown alias, got nil")
	}
}

// TestGetAccount_Found verifies that GetAccount returns the correct account.
func TestGetAccount_Found(t *testing.T) {
	withTempHome(t)

	if err := AddAccount("personal", "personal@example.com"); err != nil {
		t.Fatalf("AddAccount() failed: %v", err)
	}

	acct, err := GetAccount("personal")
	if err != nil {
		t.Fatalf("GetAccount() returned error: %v", err)
	}
	if acct.Alias != "personal" {
		t.Errorf("Alias = %q, want %q", acct.Alias, "personal")
	}
	if acct.Email != "personal@example.com" {
		t.Errorf("Email = %q, want %q", acct.Email, "personal@example.com")
	}
}
