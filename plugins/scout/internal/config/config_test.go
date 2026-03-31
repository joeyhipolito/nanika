package config

import (
	"os"
	"path/filepath"
	"testing"
)

// setConfigDir points SCOUT_CONFIG_DIR at a fresh temp directory and resets it
// after the test. All config functions re-read the env var on each call.
func setConfigDir(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	t.Setenv("SCOUT_CONFIG_DIR", dir)
	return dir
}

// TestLoadDefaults verifies that Load returns sensible defaults when no config
// file is present.
func TestLoadDefaults(t *testing.T) {
	setConfigDir(t)

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v, want nil", err)
	}
	if cfg.GatherInterval != "6h" {
		t.Errorf("GatherInterval = %q, want %q", cfg.GatherInterval, "6h")
	}
	if cfg.GeminiAPIKey != "" {
		t.Errorf("GeminiAPIKey = %q, want empty string", cfg.GeminiAPIKey)
	}
}

// TestLoadValidConfig verifies that a well-formed config file is parsed correctly.
func TestLoadValidConfig(t *testing.T) {
	dir := setConfigDir(t)
	content := "gather_interval=12h\ngemini_apikey=my-secret-key\n"
	if err := os.WriteFile(filepath.Join(dir, ConfigFileName), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v, want nil", err)
	}
	if cfg.GatherInterval != "12h" {
		t.Errorf("GatherInterval = %q, want %q", cfg.GatherInterval, "12h")
	}
	if cfg.GeminiAPIKey != "my-secret-key" {
		t.Errorf("GeminiAPIKey = %q, want %q", cfg.GeminiAPIKey, "my-secret-key")
	}
}

// TestLoadPartialConfig verifies that only the keys present in the file are
// overridden; missing keys retain their defaults.
func TestLoadPartialConfig(t *testing.T) {
	dir := setConfigDir(t)
	// Only gather_interval is set; gemini_apikey is absent.
	if err := os.WriteFile(filepath.Join(dir, ConfigFileName), []byte("gather_interval=24h\n"), 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v, want nil", err)
	}
	if cfg.GatherInterval != "24h" {
		t.Errorf("GatherInterval = %q, want %q", cfg.GatherInterval, "24h")
	}
	if cfg.GeminiAPIKey != "" {
		t.Errorf("GeminiAPIKey = %q, want empty string", cfg.GeminiAPIKey)
	}
}

// TestLoadEmptyFile verifies that an empty config file returns all defaults.
func TestLoadEmptyFile(t *testing.T) {
	dir := setConfigDir(t)
	if err := os.WriteFile(filepath.Join(dir, ConfigFileName), []byte{}, 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v, want nil", err)
	}
	if cfg.GatherInterval != "6h" {
		t.Errorf("GatherInterval = %q, want %q", cfg.GatherInterval, "6h")
	}
}

// TestLoadCorruptFile verifies that a file containing binary/non-INI data does
// not cause an error — unrecognised lines are silently skipped and defaults are
// returned for any keys that could not be parsed.
func TestLoadCorruptFile(t *testing.T) {
	dir := setConfigDir(t)
	corrupt := []byte{0xFF, 0xFE, 0x00, 0x01, 'g', 'a', 'r', 'b', 'a', 'g', 'e', 0x00}
	if err := os.WriteFile(filepath.Join(dir, ConfigFileName), corrupt, 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v, want nil for corrupt file", err)
	}
	if cfg.GatherInterval != "6h" {
		t.Errorf("GatherInterval = %q, want default %q", cfg.GatherInterval, "6h")
	}
	if cfg.GeminiAPIKey != "" {
		t.Errorf("GeminiAPIKey = %q, want empty string", cfg.GeminiAPIKey)
	}
}

// TestLoadCommentsAndBlanks verifies that comment lines and blank lines are
// ignored and do not affect parsing.
func TestLoadCommentsAndBlanks(t *testing.T) {
	dir := setConfigDir(t)
	content := "# Scout CLI Configuration\n\n# interval setting\ngather_interval=1h\n\n"
	if err := os.WriteFile(filepath.Join(dir, ConfigFileName), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v, want nil", err)
	}
	if cfg.GatherInterval != "1h" {
		t.Errorf("GatherInterval = %q, want %q", cfg.GatherInterval, "1h")
	}
}

// TestLoadUnknownKeys verifies that unrecognised keys are silently ignored and
// do not cause an error or corrupt known fields.
func TestLoadUnknownKeys(t *testing.T) {
	dir := setConfigDir(t)
	content := "gather_interval=3h\nunknown_key=some_value\nanother_key=foo\n"
	if err := os.WriteFile(filepath.Join(dir, ConfigFileName), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v, want nil", err)
	}
	if cfg.GatherInterval != "3h" {
		t.Errorf("GatherInterval = %q, want %q", cfg.GatherInterval, "3h")
	}
}

// TestSaveAndReload verifies that Save writes a file that Load can round-trip.
func TestSaveAndReload(t *testing.T) {
	setConfigDir(t)

	original := &Config{
		GatherInterval: "2h",
		GeminiAPIKey:   "test-api-key-1234",
	}
	if err := Save(original); err != nil {
		t.Fatalf("Save() error = %v", err)
	}

	loaded, err := Load()
	if err != nil {
		t.Fatalf("Load() after Save() error = %v", err)
	}
	if loaded.GatherInterval != original.GatherInterval {
		t.Errorf("GatherInterval = %q, want %q", loaded.GatherInterval, original.GatherInterval)
	}
	if loaded.GeminiAPIKey != original.GeminiAPIKey {
		t.Errorf("GeminiAPIKey = %q, want %q", loaded.GeminiAPIKey, original.GeminiAPIKey)
	}
}

// TestSaveCreatesDirectory verifies that Save creates the config directory when
// it does not already exist.
func TestSaveCreatesDirectory(t *testing.T) {
	base := t.TempDir()
	// Point to a subdirectory that doesn't exist yet.
	subdir := filepath.Join(base, "newsubdir")
	t.Setenv("SCOUT_CONFIG_DIR", subdir)

	cfg := &Config{GatherInterval: "6h"}
	if err := Save(cfg); err != nil {
		t.Fatalf("Save() error = %v", err)
	}

	if _, err := os.Stat(filepath.Join(subdir, ConfigFileName)); err != nil {
		t.Errorf("config file not found after Save: %v", err)
	}
}

// TestSaveFilePermissions verifies that the saved config file has 0600
// permissions (owner read/write only).
func TestSaveFilePermissions(t *testing.T) {
	setConfigDir(t)

	if err := Save(&Config{GatherInterval: "6h", GeminiAPIKey: "key"}); err != nil {
		t.Fatalf("Save() error = %v", err)
	}

	mode, err := Permissions()
	if err != nil {
		t.Fatalf("Permissions() error = %v", err)
	}
	if mode != 0600 {
		t.Errorf("file permissions = %04o, want 0600", mode)
	}
}

// TestExistsReturnsFalseWhenMissing verifies Exists returns false when no
// config file has been written.
func TestExistsReturnsFalseWhenMissing(t *testing.T) {
	setConfigDir(t)
	if Exists() {
		t.Error("Exists() = true, want false before any Save")
	}
}

// TestExistsReturnsTrueAfterSave verifies Exists returns true after a
// successful Save.
func TestExistsReturnsTrueAfterSave(t *testing.T) {
	setConfigDir(t)

	if err := Save(&Config{GatherInterval: "6h"}); err != nil {
		t.Fatalf("Save() error = %v", err)
	}
	if !Exists() {
		t.Error("Exists() = false, want true after Save")
	}
}

// TestPermissionsErrorWhenMissing verifies that Permissions returns an error
// when the config file does not exist.
func TestPermissionsErrorWhenMissing(t *testing.T) {
	setConfigDir(t)

	_, err := Permissions()
	if err == nil {
		t.Error("Permissions() = nil error, want error for missing file")
	}
}

// TestEnsureDirsCreatesSubdirs verifies that EnsureDirs creates both the topics
// and intel subdirectories under the config base directory.
func TestEnsureDirsCreatesSubdirs(t *testing.T) {
	dir := setConfigDir(t)

	if err := EnsureDirs(); err != nil {
		t.Fatalf("EnsureDirs() error = %v", err)
	}

	for _, sub := range []string{TopicsDirName, IntelDirName} {
		path := filepath.Join(dir, sub)
		info, err := os.Stat(path)
		if err != nil {
			t.Errorf("directory %q not created: %v", path, err)
			continue
		}
		if !info.IsDir() {
			t.Errorf("%q is not a directory", path)
		}
	}
}

// TestEnsureDirsIdempotent verifies that calling EnsureDirs twice does not
// return an error.
func TestEnsureDirsIdempotent(t *testing.T) {
	setConfigDir(t)

	if err := EnsureDirs(); err != nil {
		t.Fatalf("first EnsureDirs() error = %v", err)
	}
	if err := EnsureDirs(); err != nil {
		t.Fatalf("second EnsureDirs() error = %v", err)
	}
}

// TestPathFunctions verifies that BaseDir, Path, TopicsDir and IntelDir return
// paths rooted at SCOUT_CONFIG_DIR.
func TestPathFunctions(t *testing.T) {
	dir := setConfigDir(t)

	if got := BaseDir(); got != dir {
		t.Errorf("BaseDir() = %q, want %q", got, dir)
	}
	if got := Path(); got != filepath.Join(dir, ConfigFileName) {
		t.Errorf("Path() = %q, want %q", got, filepath.Join(dir, ConfigFileName))
	}
	if got := TopicsDir(); got != filepath.Join(dir, TopicsDirName) {
		t.Errorf("TopicsDir() = %q, want %q", got, filepath.Join(dir, TopicsDirName))
	}
	if got := IntelDir(); got != filepath.Join(dir, IntelDirName) {
		t.Errorf("IntelDir() = %q, want %q", got, filepath.Join(dir, IntelDirName))
	}
}
