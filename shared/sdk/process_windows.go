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
// remains the practical shutdown signal there.
func killProcessGroup(_ int, _ syscall.Signal) error { return nil }
