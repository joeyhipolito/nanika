package notify_test

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/notify"
)

// setupConfigDir creates a temp directory and sets ORCHESTRATOR_CONFIG_DIR so
// LoadPluginConfig reads from it. The cleanup function restores the original env var.
func setupConfigDir(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)
	return dir
}

// writePluginConfig writes cfg to <root>/channels/<plugin>.json.
func writePluginConfig(t *testing.T, root, plugin string, cfg map[string]any) {
	t.Helper()
	chDir := filepath.Join(root, "channels")
	if err := os.MkdirAll(chDir, 0700); err != nil {
		t.Fatalf("creating channels dir: %v", err)
	}
	data, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("marshaling config: %v", err)
	}
	if err := os.WriteFile(filepath.Join(chDir, plugin+".json"), data, 0600); err != nil {
		t.Fatalf("writing %s.json: %v", plugin, err)
	}
}

func TestLoadPluginConfig_NotExist(t *testing.T) {
	setupConfigDir(t)

	cfg, err := notify.LoadPluginConfig("telegram")
	if err != nil {
		t.Fatalf("want nil error when file absent, got: %v", err)
	}
	if cfg != nil {
		t.Errorf("want nil config when file absent, got: %+v", cfg)
	}
}

func TestLoadPluginConfig_Valid(t *testing.T) {
	root := setupConfigDir(t)
	writePluginConfig(t, root, "telegram", map[string]any{
		"channel_ids": []string{"12345", "67890"},
		"events":      []string{"mission.started", "mission.completed"},
	})

	cfg, err := notify.LoadPluginConfig("telegram")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if cfg == nil {
		t.Fatal("want non-nil config, got nil")
	}
	if len(cfg.ChannelIDs) != 2 {
		t.Errorf("want 2 channel IDs, got %d", len(cfg.ChannelIDs))
	}
	if len(cfg.Events) != 2 {
		t.Errorf("want 2 events, got %d", len(cfg.Events))
	}
}

func TestLoadPluginConfig_MissingChannelIDs(t *testing.T) {
	root := setupConfigDir(t)
	writePluginConfig(t, root, "discord", map[string]any{
		"channel_ids": []string{},
	})

	cfg, err := notify.LoadPluginConfig("discord")
	if err == nil {
		t.Fatal("want error for empty channel_ids, got nil")
	}
	if cfg != nil {
		t.Errorf("want nil config on error, got: %+v", cfg)
	}
}

func TestLoadPluginConfig_MalformedJSON(t *testing.T) {
	root := setupConfigDir(t)
	chDir := filepath.Join(root, "channels")
	if err := os.MkdirAll(chDir, 0700); err != nil {
		t.Fatalf("creating channels dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(chDir, "telegram.json"), []byte("{invalid json"), 0600); err != nil {
		t.Fatalf("writing bad json: %v", err)
	}

	cfg, err := notify.LoadPluginConfig("telegram")
	if err == nil {
		t.Fatal("want error for malformed JSON, got nil")
	}
	if cfg != nil {
		t.Errorf("want nil config on parse error, got: %+v", cfg)
	}
}

func TestLoadPluginConfig_EmptyEventsIsValid(t *testing.T) {
	root := setupConfigDir(t)
	writePluginConfig(t, root, "telegram", map[string]any{
		"channel_ids": []string{"12345"},
		// events omitted — caller uses defaults
	})

	cfg, err := notify.LoadPluginConfig("telegram")
	if err != nil {
		t.Fatalf("want nil error for valid minimal config, got: %v", err)
	}
	if cfg == nil {
		t.Fatal("want non-nil config, got nil")
	}
	if len(cfg.Events) != 0 {
		t.Errorf("want 0 events (caller applies defaults), got %d", len(cfg.Events))
	}
}

func TestLoadPluginConfig_UnreadableFile(t *testing.T) {
	root := setupConfigDir(t)
	chDir := filepath.Join(root, "channels")
	if err := os.MkdirAll(chDir, 0700); err != nil {
		t.Fatalf("creating channels dir: %v", err)
	}
	path := filepath.Join(chDir, "telegram.json")
	if err := os.WriteFile(path, []byte("{}"), 0600); err != nil {
		t.Fatalf("writing file: %v", err)
	}
	// Remove read permission — simulates locked/corrupt file.
	if err := os.Chmod(path, 0000); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	t.Cleanup(func() { os.Chmod(path, 0600) }) //nolint:errcheck

	// Skip if running as root (root ignores file permissions).
	if os.Getuid() == 0 {
		t.Skip("skipping permission test: running as root")
	}

	cfg, err := notify.LoadPluginConfig("telegram")
	if err == nil {
		t.Fatal("want error for unreadable file, got nil")
	}
	if cfg != nil {
		t.Errorf("want nil config on read error, got: %+v", cfg)
	}
}
