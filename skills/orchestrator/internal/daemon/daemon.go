// Package daemon implements the Nanika event relay daemon.
//
// The daemon:
//   - Acquires a PID lock (~/.alluka/daemon.pid) to prevent duplicate instances.
//   - Listens on a Unix domain socket (~/.alluka/daemon.sock) for events emitted
//     by the orchestrator engine (via UDSEmitter).
//   - Publishes received events to an in-process ring buffer bus.
//   - Serves an HTTP API (SSE + REST) from the bus for external consumers.
package daemon

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/notify"
	clinotify "github.com/joeyhipolito/orchestrator-cli/internal/notify/cli"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
)

// Config holds daemon startup parameters.
type Config struct {
	SocketPath      string // Unix domain socket path (~/.alluka/daemon.sock)
	PIDPath         string // PID file path (~/.alluka/daemon.pid)
	EventsSocketPath string // Broadcast socket path (~/.alluka/events.sock); empty disables
	APIAddr         string // TCP address for the HTTP API (e.g. "127.0.0.1:7331")
	APIKey          string // Bearer token for API auth (empty = no API key auth)
	CFTeam          string // CF Access team domain (empty = CF JWT auth disabled)
	CFAud           string // CF Access application audience tag
	AllowedEmail    string // Allowed email for CF Access JWT (enforced when CFTeam is set)
}

// Daemon is the long-running event relay process.
type Daemon struct {
	cfg      Config
	bus      *event.Bus
	listener net.Listener
}

// ValidateRemoteConfig ensures config consistency based on the mode:
//
// LOCAL MODE (APIKey only, no CFTeam):
//   - Used for localhost access with bearer token auth
//   - APIKey alone is valid; CFTeam, CFAud, AllowedEmail must all be absent
//
// REMOTE MODE (CFTeam set):
//   - Used for remote access with dual-layer auth (API key + CF JWT)
//   - All four fields required: APIKey, CFTeam, CFAud, AllowedEmail
//
// NO-AUTH MODE (all fields absent):
//   - Allows requests without any authentication
//   - Valid only for localhost-only daemons
//
// Mixing modes (e.g. APIKey + CFAud but no CFTeam) is an error.
func (cfg Config) ValidateRemoteConfig() error {
	// If CFTeam is set, we're in remote mode: all four fields required.
	if cfg.CFTeam != "" {
		missing := make([]string, 0, 3) // Pre-allocate for max 3 possible missing fields
		if cfg.APIKey == "" {
			missing = append(missing, "--api-key")
		}
		if cfg.CFAud == "" {
			missing = append(missing, "--cf-aud")
		}
		if cfg.AllowedEmail == "" {
			missing = append(missing, "--allowed-email")
		}
		if len(missing) > 0 {
			return fmt.Errorf("remote mode config incomplete; missing fields: %s", strings.Join(missing, ", "))
		}
		return nil
	}

	// CFTeam not set: local mode. APIKey alone is valid; CF fields must be absent.
	if cfg.CFAud != "" || cfg.AllowedEmail != "" {
		return fmt.Errorf("local mode config invalid: --cf-aud and --allowed-email cannot be set without --cf-team")
	}
	return nil
}

// New creates a Daemon with the given config.
func New(cfg Config) *Daemon {
	return &Daemon{
		cfg: cfg,
		bus: event.NewBus(),
	}
}

// Start acquires the PID lock, opens the UDS listener, starts the HTTP API
// server, and blocks until ctx is cancelled. On return the PID file and
// socket are cleaned up.
func (d *Daemon) Start(ctx context.Context) error {
	if err := d.cfg.ValidateRemoteConfig(); err != nil {
		return err
	}
	if err := d.acquirePID(); err != nil {
		return err
	}
	defer d.releasePID()

	// Create socket parent directory.
	if err := os.MkdirAll(filepath.Dir(d.cfg.SocketPath), 0700); err != nil {
		return fmt.Errorf("creating socket dir: %w", err)
	}
	// Remove stale socket file from a previous run.
	os.Remove(d.cfg.SocketPath) //nolint:errcheck

	ln, err := net.Listen("unix", d.cfg.SocketPath)
	if err != nil {
		return fmt.Errorf("listening on %s: %w", d.cfg.SocketPath, err)
	}
	if err := os.Chmod(d.cfg.SocketPath, 0600); err != nil {
		ln.Close()
		return fmt.Errorf("chmod socket: %w", err)
	}
	defer func() {
		ln.Close()
		os.Remove(d.cfg.SocketPath) //nolint:errcheck
	}()

	// Pre-load personas and start hot-reload watcher before serving requests.
	if err := persona.Load(); err != nil {
		fmt.Fprintf(os.Stderr, "daemon: persona load warning: %v\n", err)
	}
	persona.StartWatcher(ctx)

	// Bind HTTP API port before starting accept loop so we fail fast.
	apiSrv := NewAPIServer(d.bus, event.NewBusEmitter(d.bus), d.cfg)
	defer apiSrv.Close()
	apiLn, err := net.Listen("tcp", d.cfg.APIAddr)
	if err != nil {
		return fmt.Errorf("binding API addr %s: %w", d.cfg.APIAddr, err)
	}

	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	var wg sync.WaitGroup

	// Events broadcast socket — fans out all bus events to connected UDS clients.
	if err := d.startEventsBroadcast(ctx, &wg); err != nil {
		return fmt.Errorf("starting events broadcast: %w", err)
	}

	// Optional Telegram notifier — starts only when ~/.alluka/channels/telegram.json exists.
	tgTracker := apiSrv.addChannel(ChannelStatus{Name: "telegram", Platform: "telegram"})
	if pluginCfg, cfgErr := notify.LoadPluginConfig("telegram"); cfgErr != nil {
		tgTracker.recordError(cfgErr)
		fmt.Fprintf(os.Stderr, "daemon: telegram config: %v\n", cfgErr)
	} else if pluginCfg != nil {
		tgTracker.mu.Lock()
		tgTracker.status.Configured = true
		tgTracker.status.Active = true
		tgTracker.mu.Unlock()
		notifier := clinotify.New("telegram", pluginCfg)
		notifier.SetHook(notify.Hook{
			OnEvent: tgTracker.recordEvent,
			OnError: tgTracker.recordError,
		})
		defer notifier.Close()
		subID, subCh := d.bus.Subscribe()
		defer d.bus.Unsubscribe(subID)
		wg.Add(1)
		go func() {
			defer wg.Done()
			notifier.Consume(ctx, subCh)
		}()
		fmt.Fprintln(os.Stderr, "daemon: telegram notifier active")
	}

	// Optional Discord notifier — starts only when ~/.alluka/channels/discord.json exists.
	dcTracker := apiSrv.addChannel(ChannelStatus{Name: "discord", Platform: "discord"})
	if pluginCfg, cfgErr := notify.LoadPluginConfig("discord"); cfgErr != nil {
		dcTracker.recordError(cfgErr)
		fmt.Fprintf(os.Stderr, "daemon: discord config: %v\n", cfgErr)
	} else if pluginCfg != nil {
		dcTracker.mu.Lock()
		dcTracker.status.Configured = true
		dcTracker.status.Active = true
		dcTracker.mu.Unlock()
		notifier := clinotify.New("discord", pluginCfg)
		notifier.SetHook(notify.Hook{
			OnEvent: dcTracker.recordEvent,
			OnError: dcTracker.recordError,
		})
		defer notifier.Close()
		subID, subCh := d.bus.Subscribe()
		defer d.bus.Unsubscribe(subID)
		wg.Add(1)
		go func() {
			defer wg.Done()
			notifier.Consume(ctx, subCh)
		}()
		fmt.Fprintln(os.Stderr, "daemon: discord notifier active")
	}

	// HTTP API server goroutine.
	wg.Add(1)
	go func() {
		defer wg.Done()
		if err := apiSrv.Serve(ctx, apiLn); err != nil {
			fmt.Fprintf(os.Stderr, "daemon: API server: %v\n", err)
			cancel() // bring down the daemon if the API server dies
		}
	}()

	// Unblock Accept when ctx is cancelled.
	go func() {
		<-ctx.Done()
		ln.Close() //nolint:errcheck
	}()

	d.listener = ln
	d.acceptLoop(ctx)
	wg.Wait()
	return nil
}

// acceptLoop accepts UDS connections until ctx is cancelled.
func (d *Daemon) acceptLoop(ctx context.Context) {
	for {
		conn, err := d.listener.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return // clean shutdown
			}
			continue // transient accept error; keep going
		}
		go d.handleConn(ctx, conn)
	}
}

// handleConn reads newline-delimited JSON events from conn and publishes each
// to the bus. It exits when conn is closed or ctx is cancelled.
func (d *Daemon) handleConn(ctx context.Context, conn net.Conn) {
	defer conn.Close()
	sc := bufio.NewScanner(conn)
	sc.Buffer(make([]byte, 64*1024), 64*1024)
	for sc.Scan() {
		if ctx.Err() != nil {
			return
		}
		var ev event.Event
		if err := json.Unmarshal(sc.Bytes(), &ev); err != nil {
			continue // skip malformed lines
		}
		d.bus.Publish(ev)
	}
}

// startEventsBroadcast opens the events broadcast UDS socket and fans out every
// bus event to all connected clients as newline-delimited JSON. Consumers
// connect with `socat - UNIX-CONNECT:~/.alluka/events.sock` or
// `nc -U ~/.alluka/events.sock` to receive the live event stream.
//
// If EventsSocketPath is empty, the broadcast socket is disabled and this
// function returns nil immediately.
func (d *Daemon) startEventsBroadcast(ctx context.Context, wg *sync.WaitGroup) error {
	if d.cfg.EventsSocketPath == "" {
		return nil
	}

	if err := os.MkdirAll(filepath.Dir(d.cfg.EventsSocketPath), 0700); err != nil {
		return fmt.Errorf("creating events socket dir: %w", err)
	}
	os.Remove(d.cfg.EventsSocketPath) //nolint:errcheck

	ln, err := net.Listen("unix", d.cfg.EventsSocketPath)
	if err != nil {
		return fmt.Errorf("listening on events socket %s: %w", d.cfg.EventsSocketPath, err)
	}
	if err := os.Chmod(d.cfg.EventsSocketPath, 0600); err != nil {
		ln.Close()
		return fmt.Errorf("chmod events socket: %w", err)
	}

	// Close listener and remove socket when ctx is cancelled.
	go func() {
		<-ctx.Done()
		ln.Close()                        //nolint:errcheck
		os.Remove(d.cfg.EventsSocketPath) //nolint:errcheck
	}()

	wg.Add(1)
	go func() {
		defer wg.Done()
		for {
			conn, err := ln.Accept()
			if err != nil {
				if ctx.Err() != nil {
					return
				}
				continue
			}
			go d.handleEventsConn(ctx, conn)
		}
	}()

	fmt.Fprintf(os.Stderr, "daemon: events socket %s\n", d.cfg.EventsSocketPath)
	return nil
}

// handleEventsConn subscribes to the bus and streams events as newline-delimited
// JSON to conn until the connection is closed or ctx is cancelled.
func (d *Daemon) handleEventsConn(ctx context.Context, conn net.Conn) {
	defer conn.Close()
	subID, ch := d.bus.Subscribe()
	defer d.bus.Unsubscribe(subID)

	enc := json.NewEncoder(conn)
	for {
		select {
		case ev, ok := <-ch:
			if !ok {
				return
			}
			if err := enc.Encode(ev); err != nil {
				return
			}
		case <-ctx.Done():
			return
		}
	}
}

// acquirePID checks for a stale PID file and writes the current process PID.
func (d *Daemon) acquirePID() error {
	if err := os.MkdirAll(filepath.Dir(d.cfg.PIDPath), 0700); err != nil {
		return fmt.Errorf("creating pid dir: %w", err)
	}

	// If a PID file exists, check if the process is still alive.
	if data, err := os.ReadFile(d.cfg.PIDPath); err == nil {
		pid, parseErr := strconv.Atoi(strings.TrimSpace(string(data)))
		if parseErr == nil && isProcessAlive(pid) {
			return fmt.Errorf("daemon already running at PID %d (pid file: %s)", pid, d.cfg.PIDPath)
		}
		// Stale PID file from a crashed run; clean it up.
		os.Remove(d.cfg.PIDPath) //nolint:errcheck
	}

	f, err := os.OpenFile(d.cfg.PIDPath, os.O_CREATE|os.O_WRONLY|os.O_EXCL, 0600)
	if err != nil {
		return fmt.Errorf("writing pid file %s: %w", d.cfg.PIDPath, err)
	}
	defer f.Close()
	fmt.Fprintf(f, "%d\n", os.Getpid())
	return nil
}

func (d *Daemon) releasePID() {
	os.Remove(d.cfg.PIDPath) //nolint:errcheck
}

// isProcessAlive returns true if a process with pid exists on this system.
func isProcessAlive(pid int) bool {
	if pid <= 0 {
		return false
	}
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	// Signal 0 checks process existence without sending a signal.
	return proc.Signal(syscall.Signal(0)) == nil
}
