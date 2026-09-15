//go:build !windows

package sdk

import (
	"errors"
	"os/exec"
	"syscall"
)

// setProcessGroup configures cmd so the spawned process becomes the leader
// of a new session (and therefore a new process group with pgid == pid).
// Two reasons for the new session, not just a new process group:
//  1. The launcher kills the group on shutdown via syscall.Kill(-pid, sig),
//     so Claude and descendants that remain in the group (MCP tool servers,
//     Bash-tool grandchildren) die with the parent instead of being reparented.
//  2. When nanika runs interactively, claude must NOT inherit the parent's
//     controlling terminal. Otherwise any descendant that touches the terminal
//     (a Bash child opening /dev/tty, a runtime that probes job control)
//     receives SIGTTIN against a background process group and is stopped,
//     deadlocking the stdout pipe before tool_result is written. Setsid
//     creates a session with no controlling terminal and removes the
//     SIGTTIN/SIGTTOU delivery surface entirely. See
//     shared/artifacts/tui-plain-tool-hang-rca.md for the full diagnosis.
func setProcessGroup(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Setsid = true
}

// killProcessGroup sends sig to the process group rooted at pid. Uses the
// negative-PID convention (kill(-pid, sig)) so every process that remains in
// Claude's dedicated group receives the signal, not just Claude itself.
func killProcessGroup(pid int, sig syscall.Signal) error {
	if pid <= 0 {
		return nil
	}
	return syscall.Kill(-pid, sig)
}

// processGroupExtinct reports true only when the kernel confirms that no
// process remains in the group. EPERM and every unexpected probe error fail
// closed: they mean we cannot prove the group is gone.
func processGroupExtinct(pid int) bool {
	if pid <= 0 {
		return true
	}
	return errors.Is(syscall.Kill(-pid, 0), syscall.ESRCH)
}

func processGroupRetirementSupported() bool { return true }
