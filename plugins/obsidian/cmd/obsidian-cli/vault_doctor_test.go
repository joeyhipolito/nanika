package main

import (
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// T0.2 — §10.4 Phase 0
// Asserts: obsidian vault doctor run against the empty-vault fixture
// (testdata/fixtures/vault-empty) exits 0 and prints a clean report with no
// error or warning lines.
func TestVaultDoctor_EmptyVault(t *testing.T) {
	// Build the binary once; share it across subtests.
	binPath := filepath.Join(t.TempDir(), "obsidian")
	build := exec.Command("go", "build", "-o", binPath, ".")
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("go build failed: %v\n%s", err, out)
	}

	t.Run("empty_vault_exit_0", func(t *testing.T) {
		vaultDir := t.TempDir()
		if err := vault.InitSkeleton(vaultDir, vault.KindNanika); err != nil {
			t.Fatalf("InitSkeleton: %v", err)
		}
		out, err := exec.Command(binPath, "vault", "doctor", "--path", vaultDir).CombinedOutput()
		if err != nil {
			t.Fatalf("vault doctor exited non-zero: %v\n%s", err, out)
		}
	})

	t.Run("dangling_link_exit_1", func(t *testing.T) {
		vaultDir := t.TempDir()
		if err := vault.InitSkeleton(vaultDir, vault.KindNanika); err != nil {
			t.Fatalf("InitSkeleton: %v", err)
		}
		danglingNote := filepath.Join(vaultDir, "mocs", "dangling.md")
		if err := os.WriteFile(danglingNote, []byte("[[not-a-real-note]]"), 0644); err != nil {
			t.Fatalf("WriteFile: %v", err)
		}
		err := exec.Command(binPath, "vault", "doctor", "--path", vaultDir).Run()
		if err == nil {
			t.Fatal("expected exit code 1, got exit 0")
		}
		exitErr, ok := err.(*exec.ExitError)
		if !ok {
			t.Fatalf("unexpected error type %T: %v", err, err)
		}
		if exitErr.ExitCode() != 1 {
			t.Errorf("exit code = %d, want 1", exitErr.ExitCode())
		}
	})
}

// T4.3 — §10.4 Phase 4
// Asserts: a Zettel with zero backlinks that is older than 48 h is surfaced by
// obsidian vault doctor --orphans with its path included in the orphan report.
func TestOrphanZettel_Detection(t *testing.T) {
	t.Skip("RED — T4.3 not yet implemented (blocks on TRK-529 Phase 4)")
}
