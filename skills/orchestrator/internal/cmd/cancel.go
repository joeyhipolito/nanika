package cmd

import (
	"context"
	"fmt"
	"os"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/claims"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

func init() {
	cancelCmd := &cobra.Command{
		Use:   "cancel <workspace-id>",
		Short: "Cancel a running mission",
		Long: `Cancel a specific running mission by workspace ID.

If the orchestrator process is still alive, sends SIGTERM for graceful
shutdown (existing teardown logic skips remaining phases, emits
mission.cancelled, saves checkpoint).

If the process has already exited (background run that died, stale PID),
performs manual cleanup: marks pending phases as skipped, emits
mission.cancelled to the event log, and saves the checkpoint as cancelled.

Use --force to send SIGKILL instead of SIGTERM. After SIGKILL the cancel
command performs manual cleanup since the process cannot handle shutdown.

Examples:
  orchestrator cancel 20260316-ab12cd34
  orchestrator cancel --force 20260316-ab12cd34`,
		Args: cobra.ExactArgs(1),
		RunE: runCancel,
	}

	cancelCmd.Flags().Bool("force", false, "send SIGKILL instead of SIGTERM (then manual cleanup)")
	rootCmd.AddCommand(cancelCmd)
}

func runCancel(cmd *cobra.Command, args []string) error {
	wsID := args[0]
	force, _ := cmd.Flags().GetBool("force")

	// Resolve workspace path and validate it is inside ~/.alluka/workspaces/
	wsPath, err := core.ResolveWorkspacePath(wsID)
	if err != nil {
		return err
	}
	if err := core.ValidateWorkspacePath(wsPath); err != nil {
		return err
	}

	// Load checkpoint to check current status
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		return fmt.Errorf("loading checkpoint: %w", err)
	}

	// Refuse to cancel already-terminal missions
	switch cp.Status {
	case "completed":
		fmt.Printf("mission %s already completed\n", wsID)
		return nil
	case "failed":
		fmt.Printf("mission %s already failed\n", wsID)
		return nil
	case "cancelled":
		fmt.Printf("mission %s already cancelled\n", wsID)
		return nil
	}

	// Try to signal the running process
	pid, err := core.ReadPID(wsPath)
	if err != nil {
		return fmt.Errorf("reading pid: %w", err)
	}

	if pid > 0 {
		if signalled := signalProcess(pid, force); signalled {
			fmt.Printf("sent %s to orchestrator PID %d for mission %s\n",
				signalName(force), pid, wsID)

			if !force {
				// SIGTERM: the process handles graceful shutdown itself
				fmt.Println("the running process will handle graceful shutdown")
				return nil
			}

			// SIGKILL: process cannot handle shutdown — wait for it to die,
			// then fall through to manual cleanup.
			fmt.Println("waiting for process to exit...")
			waitForProcessExit(pid)
		} else {
			// Process is gone — fall through to manual cleanup
			fmt.Printf("orchestrator PID %d is no longer running, performing manual cleanup\n", pid)
		}
	} else {
		fmt.Printf("no PID file found for %s, performing manual cleanup\n", wsID)
	}

	// Manual cleanup: the orchestrator process is gone, so we do what it
	// would have done on SIGTERM.
	return manualCancel(wsPath, wsID, cp)
}

// signalProcess sends SIGTERM (or SIGKILL with --force) to the process.
// Returns true if the signal was delivered, false if the process is gone.
func signalProcess(pid int, force bool) bool {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}

	// Check if the process is alive (signal 0 is a no-op probe)
	if err := proc.Signal(syscall.Signal(0)); err != nil {
		return false
	}

	sig := syscall.SIGTERM
	if force {
		sig = syscall.SIGKILL
	}

	if err := proc.Signal(sig); err != nil {
		return false
	}
	return true
}

// waitForProcessExit polls until the process is no longer running (up to 5s).
func waitForProcessExit(pid int) {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return
	}
	for i := 0; i < 50; i++ { // 50 × 100ms = 5s
		if err := proc.Signal(syscall.Signal(0)); err != nil {
			return // process is gone
		}
		time.Sleep(100 * time.Millisecond)
	}
	fmt.Fprintf(os.Stderr, "warning: process %d may still be in process table after 5s\n", pid)
}

func signalName(force bool) string {
	if force {
		return "SIGKILL"
	}
	return "SIGTERM"
}

// manualCancel performs cleanup when the orchestrator process is no longer
// running. It marks pending/running phases as skipped, emits coherent events,
// and saves the checkpoint as cancelled.
func manualCancel(wsPath, wsID string, cp *core.Checkpoint) error {
	plan := cp.Plan
	if plan == nil {
		return fmt.Errorf("checkpoint has no plan")
	}

	// Open emitter for event log
	emitter, err := openCancelEmitter(wsID)
	if err != nil {
		// Non-fatal: proceed with checkpoint update even if event log fails
		fmt.Fprintf(os.Stderr, "warning: could not open event log: %v\n", err)
		emitter = event.NoOpEmitter{}
	}
	defer emitter.Close()

	ctx := context.Background()
	now := time.Now()
	skipped := 0

	// Mark non-terminal phases as skipped
	for _, phase := range plan.Phases {
		if phase.Status.IsTerminal() {
			continue
		}
		phase.Status = core.StatusSkipped
		phase.Error = "skipped: mission cancelled"
		phase.EndTime = &now
		skipped++

		emitter.Emit(ctx, event.New(event.PhaseSkipped, wsID, phase.ID, "", map[string]any{
			"reason": "mission cancelled (manual cleanup)",
		}))
	}

	// Emit mission.cancelled
	emitter.Emit(ctx, event.New(event.MissionCancelled, wsID, "", "", map[string]any{
		"source":         "orchestrator cancel",
		"skipped_phases": skipped,
	}))

	// Save checkpoint as cancelled
	if err := core.SaveCheckpoint(wsPath, plan, cp.Domain, "cancelled", cp.StartedAt); err != nil {
		return fmt.Errorf("saving checkpoint: %w", err)
	}

	// Write cancel sentinel so any zombie workers know to stop
	if err := core.WriteCancelSentinel(wsPath); err != nil {
		fmt.Fprintf(os.Stderr, "warning: could not write cancel sentinel: %v\n", err)
	}

	// Release file claims for this mission so parallel missions are unblocked.
	if cdb, cErr := claims.OpenDB(""); cErr == nil {
		_ = cdb.ReleaseAll(wsID)
		cdb.Close()
	}

	// Sync mission file status if applicable
	if synced, syncErr := core.SyncManagedMissionStatus(wsPath); syncErr != nil {
		fmt.Fprintf(os.Stderr, "warning: could not sync mission status: %v\n", syncErr)
	} else if synced != "" {
		fmt.Printf("synced status \"cancelled\" to %s\n", synced)
	}

	// Count what was already done
	completed := 0
	failed := 0
	for _, phase := range plan.Phases {
		switch phase.Status {
		case core.StatusCompleted:
			completed++
		case core.StatusFailed:
			failed++
		}
	}

	fmt.Printf("cancelled mission %s: %d completed, %d failed, %d skipped\n",
		wsID, completed, failed, skipped)

	return nil
}

// openCancelEmitter creates a FileEmitter that appends to the mission's
// existing event log.
func openCancelEmitter(wsID string) (event.Emitter, error) {
	logPath, err := event.EventLogPath(wsID)
	if err != nil {
		return nil, err
	}
	return event.NewFileEmitter(logPath)
}
