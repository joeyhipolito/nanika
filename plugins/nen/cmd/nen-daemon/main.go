// nen-daemon — standalone Nanika nen event subscriber.
//
// Subscribes to the orchestrator's HTTP SSE stream at http://127.0.0.1:7331/api/events
// and routes incoming events to the appropriate scanner binaries. Findings are
// persisted to ~/.alluka/nen/findings.db. Falls back to polling
// ~/.alluka/events/*.jsonl when the orchestrator daemon is unavailable.
//
// Usage:
//
//	nen-daemon start    Start the subscriber daemon (foreground; use & or launchd)
//	nen-daemon stop     Send SIGTERM to the running daemon
//	nen-daemon status   Print daemon PID and active finding count
package main

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
	_ "modernc.org/sqlite"
)

// Event is a minimal mirror of the orchestrator event envelope.
// Only the fields needed for routing are decoded.
type Event struct {
	ID        string         `json:"id"`
	Type      string         `json:"type"`
	Timestamp time.Time      `json:"timestamp"`
	Sequence  int64          `json:"sequence"`
	MissionID string         `json:"mission_id"`
	PhaseID   string         `json:"phase_id,omitempty"`
	WorkerID  string         `json:"worker_id,omitempty"`
	Data      map[string]any `json:"data,omitempty"`
}

// ---- Path helpers -------------------------------------------------------

func pidPath() (string, error) {
	dir, err := scan.Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "nen-daemon.pid"), nil
}

func eventsSocketPath() (string, error) {
	return scan.EventsSocketPath()
}

func findingsDBPath() (string, error) {
	dir, err := scan.Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "nen", "findings.db"), nil
}

func scannersDir() (string, error) {
	dir, err := scan.Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "nen", "scanners"), nil
}

func pluginJSONPath() (string, error) {
	dir, err := scan.Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "nen", "plugin.json"), nil
}

// ---- PID management -----------------------------------------------------

func writePID() error {
	path, err := pidPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	return os.WriteFile(path, []byte(fmt.Sprintf("%d\n", os.Getpid())), 0o600)
}

func readPID() (int, error) {
	path, err := pidPath()
	if err != nil {
		return 0, err
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return 0, err
	}
	return strconv.Atoi(strings.TrimSpace(string(data)))
}

func removePID() {
	if path, err := pidPath(); err == nil {
		os.Remove(path) //nolint:errcheck
	}
}

func isRunning() (int, bool) {
	pid, err := readPID()
	if err != nil || pid <= 0 {
		return 0, false
	}
	proc, err := os.FindProcess(pid)
	if err != nil {
		return 0, false
	}
	return pid, proc.Signal(syscall.Signal(0)) == nil
}

// ---- Findings store -----------------------------------------------------

type store struct {
	db *sql.DB
}

func openStore() (*store, error) {
	path, err := findingsDBPath()
	if err != nil {
		return nil, fmt.Errorf("findings db path: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, fmt.Errorf("create db dir: %w", err)
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, fmt.Errorf("open findings.db: %w", err)
	}
	if err := migrateStore(db); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate: %w", err)
	}
	return &store{db: db}, nil
}

func (s *store) close() error { return s.db.Close() }

func (s *store) insert(ctx context.Context, f scan.Finding) error {
	ev, _ := json.Marshal(f.Evidence)
	now := time.Now().UTC().Format(time.RFC3339)
	foundAt := f.FoundAt.UTC().Format(time.RFC3339)
	if f.FoundAt.IsZero() {
		foundAt = now
	}
	var expiresAt interface{}
	if f.ExpiresAt != nil {
		expiresAt = f.ExpiresAt.UTC().Format(time.RFC3339)
	}
	_, err := s.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO findings
			(id, ability, category, severity, title, description,
			 scope_kind, scope_value, evidence, source,
			 found_at, expires_at, superseded_by, created_at)
		VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
		f.ID, f.Ability, f.Category, string(f.Severity),
		f.Title, f.Description,
		f.Scope.Kind, f.Scope.Value,
		string(ev), f.Source,
		foundAt, expiresAt, f.SupersededBy, now,
	)
	return err
}

func (s *store) countActive(ctx context.Context) (int, error) {
	var n int
	err := s.db.QueryRowContext(ctx, `
		SELECT COUNT(*) FROM findings
		WHERE superseded_by = ''
		  AND (expires_at IS NULL OR expires_at > ?)`,
		time.Now().UTC().Format(time.RFC3339),
	).Scan(&n)
	return n, err
}

func migrateStore(db *sql.DB) error {
	stmts := []string{
		`CREATE TABLE IF NOT EXISTS nen_schema_version (
			version    INTEGER PRIMARY KEY,
			applied_at DATETIME NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS findings (
			id            TEXT PRIMARY KEY,
			ability       TEXT NOT NULL,
			category      TEXT NOT NULL,
			severity      TEXT NOT NULL,
			title         TEXT NOT NULL,
			description   TEXT NOT NULL,
			scope_kind    TEXT NOT NULL,
			scope_value   TEXT NOT NULL,
			evidence      TEXT NOT NULL DEFAULT '[]',
			source        TEXT NOT NULL,
			found_at      DATETIME NOT NULL,
			expires_at    DATETIME,
			superseded_by TEXT NOT NULL DEFAULT '',
			created_at    DATETIME NOT NULL
		)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_ability  ON findings(ability)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_severity ON findings(severity)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_found_at ON findings(found_at)`,
		`CREATE INDEX IF NOT EXISTS idx_findings_active   ON findings(superseded_by, expires_at)`,
	}
	for _, stmt := range stmts {
		if _, err := db.Exec(stmt); err != nil {
			return fmt.Errorf("exec DDL: %w", err)
		}
	}
	return nil
}

// ---- Scanner routing ----------------------------------------------------

type scannerCfg struct {
	name        string
	ability     string
	binaryPath  string
	watchRegexp *regexp.Regexp // nil = match all events
}

func (sc *scannerCfg) matches(evType string) bool {
	if sc.watchRegexp == nil {
		return true
	}
	return sc.watchRegexp.MatchString(evType)
}

func loadScanners() ([]scannerCfg, error) {
	pluginPath, err := pluginJSONPath()
	if err != nil {
		return nil, err
	}
	data, err := os.ReadFile(pluginPath)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read plugin.json: %w", err)
	}

	var p struct {
		Scanners map[string]struct {
			Ability     string `json:"ability"`
			Mode        string `json:"mode"`
			WatchEvents string `json:"watch_events"`
		} `json:"scanners"`
	}
	if err := json.Unmarshal(data, &p); err != nil {
		return nil, fmt.Errorf("parse plugin.json: %w", err)
	}

	sDir, err := scannersDir()
	if err != nil {
		return nil, err
	}

	var cfgs []scannerCfg
	for name, sc := range p.Scanners {
		if sc.Mode != "watch" {
			continue
		}
		binPath := filepath.Join(sDir, name)
		if _, err := os.Stat(binPath); err != nil {
			continue // not installed; skip silently
		}
		cfg := scannerCfg{
			name:       name,
			ability:    sc.Ability,
			binaryPath: binPath,
		}
		if sc.WatchEvents != "" {
			re, err := regexp.Compile(sc.WatchEvents)
			if err != nil {
				fmt.Fprintf(os.Stderr, "nen-daemon: compile watch_events for %s: %v\n", name, err)
				continue
			}
			cfg.watchRegexp = re
		}
		cfgs = append(cfgs, cfg)
	}
	return cfgs, nil
}

// scopeForEvent derives the scan scope from an event.
func scopeForEvent(ev Event) scan.Scope {
	switch {
	case ev.MissionID != "":
		return scan.Scope{Kind: "mission", Value: ev.MissionID}
	case ev.PhaseID != "":
		return scan.Scope{Kind: "phase", Value: ev.PhaseID}
	case ev.WorkerID != "":
		return scan.Scope{Kind: "worker", Value: ev.WorkerID}
	default:
		return scan.Scope{Kind: "event", Value: ev.Type}
	}
}

// invokeScanner runs a scanner binary with the given scope and returns its findings.
func invokeScanner(ctx context.Context, sc scannerCfg, scope scan.Scope) ([]scan.Finding, []string, error) {
	scopeJSON, _ := json.Marshal(scope)
	ctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, sc.binaryPath, "--scope", string(scopeJSON))
	out, err := cmd.Output()
	if err != nil {
		return nil, nil, fmt.Errorf("run scanner: %w", err)
	}
	var env scan.Envelope
	if err := json.Unmarshal(out, &env); err != nil {
		return nil, nil, fmt.Errorf("parse envelope: %w", err)
	}
	return env.Findings, env.Warnings, nil
}

// routeEvent dispatches an event to all matching scanners and stores findings.
func routeEvent(ctx context.Context, ev Event, scanners []scannerCfg, st *store) {
	scope := scopeForEvent(ev)
	for _, sc := range scanners {
		if !sc.matches(ev.Type) {
			continue
		}
		findings, warnings, err := invokeScanner(ctx, sc, scope)
		if err != nil {
			fmt.Fprintf(os.Stderr, "nen-daemon: scanner %s (event %s): %v\n", sc.name, ev.Type, err)
			continue
		}
		for _, w := range warnings {
			fmt.Fprintf(os.Stderr, "nen-daemon: scanner %s warning: %s\n", sc.name, w)
		}
		for _, f := range findings {
			if err := st.insert(ctx, f); err != nil {
				fmt.Fprintf(os.Stderr, "nen-daemon: store finding %s: %v\n", f.ID, err)
			}
		}
	}
}

// ---- Event sources ------------------------------------------------------

// readUDS connects to socketPath and streams NDJSON events into ch.
// Returns nil when ctx is cancelled; returns an error if the initial
// connection fails (so the caller can fall back to JSONL polling).
func readUDS(ctx context.Context, socketPath string, ch chan<- Event) error {
	conn, err := net.DialTimeout("unix", socketPath, 5*time.Second)
	if err != nil {
		return err // unavailable — signal fallback
	}

	fmt.Fprintln(os.Stderr, "nen-daemon: connected to events.sock")

	go func() {
		<-ctx.Done()
		conn.Close() //nolint:errcheck
	}()

	sc := bufio.NewScanner(conn)
	sc.Buffer(make([]byte, 64*1024), 64*1024)
	for sc.Scan() {
		if ctx.Err() != nil {
			return nil
		}
		var ev Event
		if err := json.Unmarshal(sc.Bytes(), &ev); err != nil {
			continue
		}
		select {
		case ch <- ev:
		case <-ctx.Done():
			return nil
		}
	}
	return nil
}

// readUDSWithReconnect repeatedly connects to socketPath, reconnecting with
// exponential backoff after drops. Sends all events to ch.
// Falls back to JSONL polling if the initial connection fails.
func readUDSWithReconnect(ctx context.Context, socketPath string, ch chan<- Event) bool {
	// Probe first to decide if UDS is available at all.
	conn, err := net.DialTimeout("unix", socketPath, 5*time.Second)
	if err != nil {
		return false // trigger JSONL fallback
	}
	conn.Close() //nolint:errcheck

	backoff := time.Second
	for ctx.Err() == nil {
		if err := readUDS(ctx, socketPath, ch); err != nil {
			// First connect attempt failed after probe — unusual; fall back.
			return false
		}
		if ctx.Err() != nil {
			return true
		}
		fmt.Fprintf(os.Stderr, "nen-daemon: events.sock dropped, reconnecting in %v\n", backoff)
		select {
		case <-time.After(backoff):
		case <-ctx.Done():
			return true
		}
		if backoff < 60*time.Second {
			backoff *= 2
		}
	}
	return true
}

// pollJSONL tails new events from ~/.alluka/events/*.jsonl files,
// starting from the current end of each file (skipping existing history).
func pollJSONL(ctx context.Context, ch chan<- Event) {
	dir, err := scan.EventsDir()
	if err != nil {
		fmt.Fprintf(os.Stderr, "nen-daemon: events dir: %v\n", err)
		return
	}

	fmt.Fprintln(os.Stderr, "nen-daemon: falling back to JSONL polling")

	// Seed offsets at current EOF so we only tail new events.
	offsets := make(map[string]int64)
	if entries, err := os.ReadDir(dir); err == nil {
		for _, e := range entries {
			if e.IsDir() || !strings.HasSuffix(e.Name(), ".jsonl") {
				continue
			}
			path := filepath.Join(dir, e.Name())
			if info, err := os.Stat(path); err == nil {
				offsets[path] = info.Size()
			}
		}
	}

	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			entries, err := os.ReadDir(dir)
			if err != nil {
				continue
			}
			for _, e := range entries {
				if e.IsDir() || !strings.HasSuffix(e.Name(), ".jsonl") {
					continue
				}
				path := filepath.Join(dir, e.Name())
				newOffset, events := readJSONLFrom(path, offsets[path])
				offsets[path] = newOffset
				for _, ev := range events {
					select {
					case ch <- ev:
					case <-ctx.Done():
						return
					}
				}
			}
		}
	}
}

// readJSONLFrom reads events from path starting at the given byte offset.
// Returns the new offset after the last successfully read line.
func readJSONLFrom(path string, offset int64) (int64, []Event) {
	f, err := os.Open(path)
	if err != nil {
		return offset, nil
	}
	defer f.Close()

	if _, err := f.Seek(offset, 0); err != nil {
		return offset, nil
	}

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 256*1024), 256*1024)

	var events []Event
	pos := offset
	for sc.Scan() {
		line := sc.Bytes()
		pos += int64(len(line)) + 1 // +1 for the newline byte
		var ev Event
		if err := json.Unmarshal(line, &ev); err != nil {
			continue
		}
		events = append(events, ev)
	}
	return pos, events
}

// ---- Daemon main loop ---------------------------------------------------

func runDaemon(ctx context.Context) error {
	if err := writePID(); err != nil {
		return fmt.Errorf("write pid: %w", err)
	}
	defer removePID()

	st, err := openStore()
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	defer st.close()

	scanners, err := loadScanners()
	if err != nil {
		fmt.Fprintf(os.Stderr, "nen-daemon: load scanners: %v\n", err)
	}
	fmt.Fprintf(os.Stderr, "nen-daemon: %d scanner(s) loaded\n", len(scanners))

	socketPath, _ := eventsSocketPath()

	ch := make(chan Event, 256)

	go func() {
		defer close(ch)
		if socketPath != "" && readUDSWithReconnect(ctx, socketPath, ch) {
			return // UDS handled everything (or ctx cancelled)
		}
		if ctx.Err() != nil {
			return
		}
		pollJSONL(ctx, ch)
	}()

	fmt.Fprintf(os.Stderr, "nen-daemon: running (PID %d)\n", os.Getpid())

	for {
		select {
		case ev, ok := <-ch:
			if !ok {
				return nil
			}
			routeEvent(ctx, ev, scanners, st)
		case <-ctx.Done():
			return nil
		}
	}
}

// ---- CLI commands -------------------------------------------------------

func cmdStart() error {
	if pid, running := isRunning(); running {
		return fmt.Errorf("already running (PID %d)", pid)
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	return runDaemon(ctx)
}

func cmdStop() error {
	pid, running := isRunning()
	if !running {
		return fmt.Errorf("not running")
	}
	proc, err := os.FindProcess(pid)
	if err != nil {
		return fmt.Errorf("find process %d: %w", pid, err)
	}
	if err := proc.Signal(syscall.SIGTERM); err != nil {
		return fmt.Errorf("signal %d: %w", pid, err)
	}
	fmt.Printf("nen-daemon: sent SIGTERM to PID %d\n", pid)
	return nil
}

func cmdStatus() error {
	pid, running := isRunning()
	if !running {
		fmt.Println("nen-daemon: not running")
		return nil
	}
	fmt.Printf("nen-daemon: running (PID %d)\n", pid)

	st, err := openStore()
	if err != nil {
		return nil // DB not yet created is non-fatal for status
	}
	defer st.close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if n, err := st.countActive(ctx); err == nil {
		fmt.Printf("nen-daemon: %d active finding(s)\n", n)
	}
	return nil
}

func main() {
	args := os.Args[1:]
	if len(args) == 0 {
		fmt.Fprintf(os.Stderr, "Usage: nen-daemon <start|stop|status>\n")
		os.Exit(1)
	}

	var err error
	switch args[0] {
	case "start":
		err = cmdStart()
	case "stop":
		err = cmdStop()
	case "status":
		err = cmdStatus()
	default:
		fmt.Fprintf(os.Stderr, "nen-daemon: unknown command %q\nUsage: nen-daemon <start|stop|status>\n", args[0])
		os.Exit(1)
	}

	if err != nil {
		fmt.Fprintf(os.Stderr, "nen-daemon %s: %v\n", args[0], err)
		os.Exit(1)
	}
}
