//go:build !windows

package sdk

import (
	"os/exec"
	"syscall"
)

// setProcessGroup configures cmd so the spawned process becomes the leader
// of a new session (and therefore a new process group with pgid == pid).
// Two reasons for the new session, not just a new process group:
//  1. The launcher kills the group on shutdown via syscall.Kill(-pid, sig),
//     so claude and its descendants (MCP tool servers, Bash-tool grandchildren)
//     die with the parent instead of being reparented to init.
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
// negative-PID convention (kill(-pid, sig)) so every descendant of the
// claude root process receives the signal, not just claude itself.
func killProcessGroup(pid int, sig syscall.Signal) error {
	if pid <= 0 {
		return nil
	}
	return syscall.Kill(-pid, sig)
}
