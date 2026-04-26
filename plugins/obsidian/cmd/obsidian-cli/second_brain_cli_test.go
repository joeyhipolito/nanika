package main

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// TestCLI_SecondBrain_CaptureAndDoctor exercises the full CLI dispatch path with
// --vault second-brain. It catches the two classes of bug the in-process unit
// tests cannot see:
//  1. the CLI failing to thread vault.Kind into subcommand dispatch, and
//  2. a subcommand (like doctor) running before the --vault flag is resolved.
//
// Setup mirrors a real install: a temp config dir with second_brain_path set,
// two temp vault dirs, and the real obsidian binary built from source.
func TestCLI_SecondBrain_CaptureAndDoctor(t *testing.T) {
	// Build the binary once; share it across assertions.
	binPath := filepath.Join(t.TempDir(), "obsidian")
	build := exec.Command("go", "build", "-o", binPath, ".")
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("go build failed: %v\n%s", err, out)
	}

	configDir := t.TempDir()
	nanikaVault := t.TempDir()
	secondBrainVault := t.TempDir()

	if err := vault.InitSkeleton(nanikaVault, vault.KindNanika); err != nil {
		t.Fatalf("InitSkeleton nanika: %v", err)
	}
	if err := vault.InitSkeleton(secondBrainVault, vault.KindSecondBrain); err != nil {
		t.Fatalf("InitSkeleton second-brain: %v", err)
	}

	// Write a config that points at both vaults. Include a dummy API key so
	// doctor's "Gemini API key" check doesn't fail the run.
	configContent := "gemini_apikey=test-key\nvault_path=" + nanikaVault + "\nsecond_brain_path=" + secondBrainVault + "\n"
	configPath := filepath.Join(configDir, "config")
	if err := os.WriteFile(configPath, []byte(configContent), 0600); err != nil {
		t.Fatalf("write config: %v", err)
	}

	runCLI := func(t *testing.T, args ...string) (string, error) {
		t.Helper()
		c := exec.Command(binPath, args...)
		c.Env = append(os.Environ(), "OBSIDIAN_CONFIG_DIR="+configDir)
		out, err := c.CombinedOutput()
		return string(out), err
	}

	t.Run("capture_default_lands_in_nanika_inbox", func(t *testing.T) {
		out, err := runCLI(t, "capture", "default-vault-probe")
		if err != nil {
			t.Fatalf("capture (default) failed: %v\n%s", err, out)
		}
		entries, err := os.ReadDir(filepath.Join(nanikaVault, vault.NanikaInbox))
		if err != nil {
			t.Fatalf("read nanika inbox: %v", err)
		}
		if len(entries) != 1 {
			t.Fatalf("expected 1 note in nanika inbox, got %d", len(entries))
		}
	})

	t.Run("capture_second_brain_lands_in_second_brain_inbox", func(t *testing.T) {
		out, err := runCLI(t, "capture", "second-brain-probe", "--vault", "second-brain")
		if err != nil {
			t.Fatalf("capture --vault second-brain failed: %v\n%s", err, out)
		}
		entries, err := os.ReadDir(filepath.Join(secondBrainVault, vault.SecondBrainInbox))
		if err != nil {
			t.Fatalf("read second-brain inbox: %v", err)
		}
		if len(entries) != 1 {
			t.Fatalf("expected 1 note in second-brain inbox, got %d", len(entries))
		}
		// Must NOT also land in the nanika inbox (that would mean kind did not thread).
		nanikaEntries, _ := os.ReadDir(filepath.Join(nanikaVault, vault.NanikaInbox))
		if len(nanikaEntries) != 1 {
			t.Errorf("second-brain capture leaked into nanika inbox: got %d nanika entries, want 1", len(nanikaEntries))
		}
	})

	t.Run("doctor_second_brain_inspects_second_brain_path", func(t *testing.T) {
		out, err := runCLI(t, "doctor", "--vault", "second-brain")
		if err != nil {
			t.Fatalf("doctor --vault second-brain failed: %v\n%s", err, out)
		}
		// The "Second-brain path" label only appears in doctor's output when
		// it resolved the kind correctly; a dispatch-ordering regression would
		// show "Vault path" instead.
		if !strings.Contains(out, "Second-brain path") {
			t.Errorf("doctor did not report Second-brain path; --vault flag lost in dispatch.\nOutput:\n%s", out)
		}
		if !strings.Contains(out, secondBrainVault) {
			t.Errorf("doctor did not inspect the second-brain path %q.\nOutput:\n%s", secondBrainVault, out)
		}
	})

	t.Run("unknown_vault_flag_errors", func(t *testing.T) {
		out, err := runCLI(t, "capture", "bogus", "--vault", "not-a-vault")
		if err == nil {
			t.Fatalf("expected non-zero exit for unknown --vault value\nOutput:\n%s", out)
		}
		if !strings.Contains(out, "unknown --vault") {
			t.Errorf("expected 'unknown --vault' error message, got:\n%s", out)
		}
	})
}
