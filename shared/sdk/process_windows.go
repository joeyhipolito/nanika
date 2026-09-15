//go:build windows

package sdk

import (
	"os/exec"
	"syscall"
)

// setProcessGroup is a no-op on Windows. Process groups are not portable
// to the Win32 Job Object model and the TUI launcher has no Windows users
// in production. Left as a stub so the cross-platform spawn site stays
// readable.
func setProcessGroup(_ *exec.Cmd) {}

// killProcessGroup is a no-op on Windows. The launcher's stdin-close path
// is followed by an explicit root Process.Kill in Query retirement.
func killProcessGroup(_ int, _ syscall.Signal) error { return nil }

// processGroupExtinct is a portability stub used only by shared teardown code.
// Callers must consult processGroupRetirementSupported before treating it as a
// process-group retirement proof.
func processGroupExtinct(_ int) bool { return true }

func processGroupRetirementSupported() bool { return false }
