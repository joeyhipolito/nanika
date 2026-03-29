package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/daemon"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

func init() {
	daemonCmd := &cobra.Command{
		Use:   "daemon",
		Short: "Manage the event relay daemon",
		Long: `The daemon relays orchestrator events to external consumers via SSE.

It listens on a Unix domain socket for events from the orchestrator engine
and fans them out to connected HTTP clients via Server-Sent Events.

Routes:
  GET /api/events                      — SSE stream (Last-Event-ID reconnect, 15s keepalive)
  GET /api/missions                    — list known mission logs
  GET /api/missions/{id}               — single mission detail + plan overview
  GET /api/missions/{id}/dag           — execution DAG (nodes=phases, edges=dependencies)
  GET /api/missions/{id}/events        — replay a mission's event log
  GET /api/health                      — health check (unauthenticated)`,
	}

	var (
		port         int
		apiKey       string
		cfTeam       string
		cfAud        string
		allowedEmail string
	)

	startCmd := &cobra.Command{
		Use:   "start",
		Short: "Start the daemon (runs in foreground)",
		Long: `Start the event relay daemon.

The daemon binds a Unix domain socket (~/.alluka/daemon.sock) for receiving
events from the orchestrator and an HTTP port for SSE consumers.

Run in the background with: orchestrator daemon start &
Stop with: orchestrator daemon stop`,
		RunE: func(cmd *cobra.Command, args []string) error {
			return runDaemonStart(port, apiKey, cfTeam, cfAud, allowedEmail)
		},
	}
	startCmd.Flags().IntVar(&port, "port", 7331, "HTTP API port")
	startCmd.Flags().StringVar(&apiKey, "api-key", "", "API key for Bearer token authentication")
	startCmd.Flags().StringVar(&cfTeam, "cf-team", "", "CF Access team domain (e.g. myteam.cloudflareaccess.com)")
	startCmd.Flags().StringVar(&cfAud, "cf-aud", "", "CF Access application audience tag")
	startCmd.Flags().StringVar(&allowedEmail, "allowed-email", "", "Allowed email for CF Access JWT verification")

	stopCmd := &cobra.Command{
		Use:   "stop",
		Short: "Send SIGTERM to the running daemon",
		RunE:  runDaemonStop,
	}

	statusCmd := &cobra.Command{
		Use:   "status",
		Short: "Show daemon status",
		RunE:  runDaemonStatus,
	}

	daemonCmd.AddCommand(startCmd, stopCmd, statusCmd)
	rootCmd.AddCommand(daemonCmd)
}

func runDaemonStart(port int, apiKey, cfTeam, cfAud, allowedEmail string) error {
	sockPath, err := event.DaemonSocketPath()
	if err != nil {
		return fmt.Errorf("resolving socket path: %w", err)
	}
	pidPath, err := event.DaemonPIDPath()
	if err != nil {
		return fmt.Errorf("resolving pid path: %w", err)
	}
	eventsPath, err := event.EventsSocketPath()
	if err != nil {
		return fmt.Errorf("resolving events socket path: %w", err)
	}

	cfg := daemon.Config{
		SocketPath:       sockPath,
		PIDPath:          pidPath,
		EventsSocketPath: eventsPath,
		APIAddr:          fmt.Sprintf("127.0.0.1:%d", port),
		APIKey:           apiKey,
		CFTeam:           cfTeam,
		CFAud:            cfAud,
		AllowedEmail:     allowedEmail,
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigCh
		fmt.Fprintln(os.Stderr, "daemon: shutting down...")
		cancel()
	}()

	fmt.Printf("daemon: socket %s\n", sockPath)
	fmt.Printf("daemon: events %s\n", eventsPath)
	fmt.Printf("daemon: api    http://127.0.0.1:%d\n", port)

	d := daemon.New(cfg)
	return d.Start(ctx)
}

func runDaemonStop(cmd *cobra.Command, args []string) error {
	pidPath, err := event.DaemonPIDPath()
	if err != nil {
		return fmt.Errorf("resolving pid path: %w", err)
	}

	data, err := os.ReadFile(pidPath)
	if os.IsNotExist(err) {
		fmt.Println("daemon: not running (no pid file)")
		return nil
	}
	if err != nil {
		return fmt.Errorf("reading pid file: %w", err)
	}

	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil {
		return fmt.Errorf("parsing pid from %s: %w", pidPath, err)
	}

	proc, err := os.FindProcess(pid)
	if err != nil {
		return fmt.Errorf("finding process %d: %w", pid, err)
	}
	if err := proc.Signal(syscall.SIGTERM); err != nil {
		return fmt.Errorf("sending SIGTERM to PID %d: %w", pid, err)
	}

	fmt.Printf("daemon: sent SIGTERM to PID %d\n", pid)
	return nil
}

func runDaemonStatus(cmd *cobra.Command, args []string) error {
	pidPath, err := event.DaemonPIDPath()
	if err != nil {
		return fmt.Errorf("resolving pid path: %w", err)
	}

	data, err := os.ReadFile(pidPath)
	if os.IsNotExist(err) {
		fmt.Println("daemon: not running")
		return nil
	}
	if err != nil {
		return fmt.Errorf("reading pid file: %w", err)
	}

	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil {
		fmt.Printf("daemon: pid file %s contains invalid data\n", pidPath)
		return nil
	}

	proc, err := os.FindProcess(pid)
	if err != nil {
		fmt.Printf("daemon: pid %d (stale pid file, process not found)\n", pid)
		return nil
	}
	if err := proc.Signal(syscall.Signal(0)); err != nil {
		fmt.Printf("daemon: pid %d (stale — process not running)\n", pid)
		return nil
	}

	fmt.Printf("daemon: running (PID %d)\n", pid)
	return nil
}
