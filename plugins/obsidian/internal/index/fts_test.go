package index

import (
	"database/sql"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
)

// T0.3 — §10.4 Phase 0
// Asserts: running obsidian index rebuild on an empty vault (testdata/fixtures/
// vault-empty) writes a valid but empty index.db, an empty graph.bin, and an
// empty preflight.md under .cache/.
func TestIndexRebuild_EmptyVault(t *testing.T) {
	vaultPath := t.TempDir() + "/vault"
	cachePath := t.TempDir() + "/cache"

	if err := RebuildEmpty(vaultPath, cachePath); err != nil {
		t.Fatalf("RebuildEmpty: %v", err)
	}

	t.Run("index_db_has_tables", func(t *testing.T) {
		dbPath := filepath.Join(cachePath, "index.db")
		db, err := sql.Open("sqlite", dbPath)
		if err != nil {
			t.Fatalf("sql.Open: %v", err)
		}
		defer db.Close()
		var count int
		if err := db.QueryRow("SELECT count(*) FROM sqlite_master WHERE type='table'").Scan(&count); err != nil {
			t.Fatalf("query sqlite_master: %v", err)
		}
		if count < 1 {
			t.Errorf("expected ≥1 table in index.db, got %d", count)
		}
	})

	t.Run("graph_bin_12_bytes", func(t *testing.T) {
		graphPath := filepath.Join(cachePath, "graph.bin")
		info, err := os.Stat(graphPath)
		if err != nil {
			t.Fatalf("stat graph.bin: %v", err)
		}
		const want = 12 // len("CSR1") + uint32 nodes + uint32 edges
		if info.Size() != want {
			t.Errorf("graph.bin size = %d, want %d", info.Size(), want)
		}
	})

	t.Run("preflight_md_nonempty_and_bounded", func(t *testing.T) {
		preflightPath := filepath.Join(cachePath, "preflight.md")
		data, err := os.ReadFile(preflightPath)
		if err != nil {
			t.Fatalf("ReadFile preflight.md: %v", err)
		}
		if len(data) == 0 {
			t.Error("preflight.md is empty")
		}
		if len(data) > 1024 {
			t.Errorf("preflight.md too large: %d bytes (max 1024)", len(data))
		}
	})
}

// T0.4 — §10.4 Phase 0
// Asserts: both vault_path and second_brain_path load correctly from
// ~/.obsidian/config; missing keys produce a clear, actionable error message
// rather than a panic or silent zero-value.
func TestConfigResolution(t *testing.T) {
	t.Run("happy_path_absolute", func(t *testing.T) {
		dir := t.TempDir()
		content := "vault_path = /abs/path/vault\nsecond_brain_path = /abs/path/sb\n"
		if err := os.WriteFile(filepath.Join(dir, "config"), []byte(content), 0644); err != nil {
			t.Fatal(err)
		}
		t.Setenv("OBSIDIAN_CONFIG_DIR", dir)
		store := config.NewStoreWithEnv("OBSIDIAN_CONFIG_DIR")

		vp, err := store.VaultPath()
		if err != nil {
			t.Fatalf("VaultPath: %v", err)
		}
		if vp != "/abs/path/vault" {
			t.Errorf("VaultPath = %q, want /abs/path/vault", vp)
		}

		sbp, err := store.SecondBrainPath()
		if err != nil {
			t.Fatalf("SecondBrainPath: %v", err)
		}
		if sbp != "/abs/path/sb" {
			t.Errorf("SecondBrainPath = %q, want /abs/path/sb", sbp)
		}
	})

	t.Run("missing_vault_path_error", func(t *testing.T) {
		dir := t.TempDir()
		content := "second_brain_path = /abs/path/sb\n"
		if err := os.WriteFile(filepath.Join(dir, "config"), []byte(content), 0644); err != nil {
			t.Fatal(err)
		}
		t.Setenv("OBSIDIAN_CONFIG_DIR", dir)
		store := config.NewStoreWithEnv("OBSIDIAN_CONFIG_DIR")

		_, err := store.VaultPath()
		if err == nil {
			t.Fatal("expected error for missing vault_path, got nil")
		}
		if !strings.Contains(err.Error(), `config key "vault_path" missing in`) {
			t.Errorf("unexpected error message: %v", err)
		}
	})

	t.Run("relative_vault_path_resolved_against_home", func(t *testing.T) {
		dir := t.TempDir()
		fakeHome := t.TempDir()
		content := "vault_path = vault/\n"
		if err := os.WriteFile(filepath.Join(dir, "config"), []byte(content), 0644); err != nil {
			t.Fatal(err)
		}
		t.Setenv("OBSIDIAN_CONFIG_DIR", dir)
		t.Setenv("HOME", fakeHome)
		store := config.NewStoreWithEnv("OBSIDIAN_CONFIG_DIR")

		vp, err := store.VaultPath()
		if err != nil {
			t.Fatalf("VaultPath: %v", err)
		}
		want := filepath.Join(fakeHome, "vault")
		if vp != want {
			t.Errorf("VaultPath = %q, want %q", vp, want)
		}
	})
}
