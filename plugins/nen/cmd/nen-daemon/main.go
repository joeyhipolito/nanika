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
	"bytes"
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
	"sort"
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

// scannerStat tracks per-scanner routing metrics.
type scannerStat struct {
	EventsRouted int64      `json:"events_routed"`
	ErrorCount   int64      `json:"error_count"`
	LastError    string     `json:"last_error,omitempty"`
	LastErrorAt  *time.Time `json:"last_error_at,omitempty"`
}

// daemonStats is written to disk by the running daemon so the status
// subcommand can read it without inter-process communication.
type daemonStats struct {
	StartedAt      time.Time               `json:"started_at"`
	TotalEvents    int64                   `json:"total_events"`
	LastEventAt    *time.Time              `json:"last_event_at,omitempty"`
	ConnectionMode string                  `json:"connection_mode"` // "uds" or "jsonl"
	Scanners       map[string]*scannerStat `json:"scanners"`
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

func statsFilePath() (string, error) {
	dir, err := scan.Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "nen", "nen-daemon.stats.json"), nil
}

// ---- Stats persistence --------------------------------------------------

// writeStats atomically writes stats to the stats file via a temp+rename.
func writeStats(stats *daemonStats) {
	path, err := statsFilePath()
	if err != nil {
		return
	}
	data, err := json.Marshal(stats)
	if err != nil {
		return
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, data, 0o600); err != nil {
		return
	}
	os.Rename(tmp, path) //nolint:errcheck
}

// readStats reads the stats file written by the running daemon.
func readStats() (daemonStats, error) {
	path, err := statsFilePath()
	if err != nil {
		return daemonStats{}, err
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return daemonStats{}, err
	}
	var s daemonStats
	if err := json.Unmarshal(data, &s); err != nil {
		return daemonStats{}, fmt.Errorf("parse stats: %w", err)
	}
	return s, nil
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
	pid, err := strconv.Atoi(strings.TrimSpace(string(data)))
	if err != nil {
		// Garbage in PID file — remove it so startup can proceed.
		removePIDFile()
		return 0, fmt.Errorf("pid file contains non-numeric data: %w", err)
	}
	return pid, nil
}

func removePIDFile() {
	if path, err := pidPath(); err == nil {
		os.Remove(path) //nolint:errcheck
	}
}

// processNameForPID returns the comm name of the process with the given PID
// using `ps -p <pid> -o comm=`. Returns empty string if the process is not found.
func processNameForPID(pid int) string {
	out, err := exec.Command("ps", "-p", strconv.Itoa(pid), "-o", "comm=").Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}

func isRunning() (int, bool) {
	pid, err := readPID()
	if err != nil || pid <= 0 {
		return 0, false
	}
	name := processNameForPID(pid)
	if name == "" {
		// PID does not exist — stale file.
		removePIDFile()
		return 0, false
	}
	if !strings.Contains(name, "nen-daemon") {
		// PID was recycled by another process — stale file.
		removePIDFile()
		return 0, false
	}
	return pid, true
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

// insert routes through scan.UpsertFinding so daemon ingestion shares the same
// semantic-key dedup logic as PersistFindings. See TRK-382.
func (s *store) insert(ctx context.Context, f scan.Finding) error {
	return scan.UpsertFinding(ctx, s.db, f)
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
		// Covers the semantic-key dedup lookup in scan.UpsertFinding.
		`CREATE INDEX IF NOT EXISTS idx_findings_identity ON findings(ability, category, scope_kind, scope_value, superseded_by)`,
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
	var stderrBuf bytes.Buffer
	cmd.Stderr = &stderrBuf
	out, err := cmd.Output()
	if err != nil {
		if stderrBuf.Len() > 0 {
			fmt.Fprintf(os.Stderr, "nen-daemon: scanner %s stderr: %s\n", sc.name, strings.TrimRight(stderrBuf.String(), "\n"))
		}
		return nil, nil, fmt.Errorf("run scanner: %w", err)
	}
	var env scan.Envelope
	if err := json.Unmarshal(out, &env); err != nil {
		return nil, nil, fmt.Errorf("parse envelope: %w", err)
	}
	return env.Findings, env.Warnings, nil
}

// scannerStatFor returns the scannerStat for name, creating it if absent.
func scannerStatFor(stats *daemonStats, name string) *scannerStat {
	ss := stats.Scanners[name]
	if ss == nil {
		ss = &scannerStat{}
		stats.Scanners[name] = ss
	}
	return ss
}

// routeEvent dispatches an event to all matching scanners and stores findings.
// stats is updated in place; caller is responsible for persisting it.
func routeEvent(ctx context.Context, ev Event, scanners []scannerCfg, st *store, stats *daemonStats) {
	scope := scopeForEvent(ev)
	for _, sc := range scanners {
		if !sc.matches(ev.Type) {
			continue
		}
		findings, warnings, err := invokeScanner(ctx, sc, scope)
		if err != nil {
			fmt.Fprintf(os.Stderr, "nen-daemon: scanner %s (event %s): %v\n", sc.name, ev.Type, err)
			ss := scannerStatFor(stats, sc.name)
			ss.ErrorCount++
			ss.LastError = err.Error()
			now := time.Now().UTC()
			ss.LastErrorAt = &now
			continue
		}
		ss := scannerStatFor(stats, sc.name)
		ss.EventsRouted++
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
// Sends "uds" to connModeCh on successful connect.
// Returns nil when ctx is cancelled; returns an error if the initial
// connection fails (so the caller can fall back to JSONL polling).
func readUDS(ctx context.Context, socketPath string, ch chan<- Event, connModeCh chan<- string) error {
	conn, err := net.DialTimeout("unix", socketPath, 5*time.Second)
	if err != nil {
		return err // unavailable — signal fallback
	}

	fmt.Fprintln(os.Stderr, "nen-daemon: connected to events.sock")
	select {
	case connModeCh <- "uds":
	default:
	}

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

// readUDSWithReconnect repeatedly connects to socketPath. On initial failure
// or after a drop, it falls back to JSONL polling for the backoff duration
// (1s, 2s, 4s, 8s, …, max 30s) then retries. Logs each reconnect attempt.
// Never permanently falls back to JSONL while ctx is alive.
// Sends "uds" or "jsonl" to connModeCh whenever the active mode changes.
func readUDSWithReconnect(ctx context.Context, socketPath string, ch chan<- Event, connModeCh chan<- string) {
	const maxBackoff = 30 * time.Second
	backoff := time.Second

	for ctx.Err() == nil {
		fmt.Fprintf(os.Stderr, "nen-daemon: connecting to events.sock (backoff %v)\n", backoff)

		err := readUDS(ctx, socketPath, ch, connModeCh)
		if ctx.Err() != nil {
			return
		}

		if err != nil {
			fmt.Fprintf(os.Stderr, "nen-daemon: UDS unavailable (%v), JSONL fallback active, retry in %v\n", err, backoff)
		} else {
			// Connected but then dropped — reset backoff and log.
			fmt.Fprintf(os.Stderr, "nen-daemon: events.sock dropped, JSONL fallback active, reconnecting in %v\n", backoff)
			backoff = time.Second
		}

		// Signal JSONL fallback mode.
		select {
		case connModeCh <- "jsonl":
		default:
		}

		// Run JSONL polling for the backoff duration to fill the gap.
		tctx, cancel := context.WithTimeout(ctx, backoff)
		pollJSONL(tctx, ch)
		cancel()

		if ctx.Err() != nil {
			return
		}

		if backoff < maxBackoff {
			backoff *= 2
			if backoff > maxBackoff {
				backoff = maxBackoff
			}
		}
	}
}

// pollJSONL tails new events from ~/.alluka/events/*.jsonl files,
// starting from the current end of each file (skipping existing history).
func pollJSONL(ctx context.Context, ch chan<- Event) {
	dir, err := scan.EventsDir()
	if err != nil {
		fmt.Fprintf(os.Stderr, "nen-daemon: events dir: %v\n", err)
		return
	}

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
	defer removePIDFile()

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
	connModeCh := make(chan string, 4)

	go func() {
		defer close(ch)
		if socketPath != "" {
			readUDSWithReconnect(ctx, socketPath, ch, connModeCh)
			return
		}
		fmt.Fprintln(os.Stderr, "nen-daemon: no events.sock configured, using JSONL polling")
		select {
		case connModeCh <- "jsonl":
		default:
		}
		pollJSONL(ctx, ch)
	}()

	stats := daemonStats{
		StartedAt:      time.Now().UTC(),
		ConnectionMode: "jsonl", // default until UDS connects
		Scanners:       make(map[string]*scannerStat),
	}
	writeStats(&stats)

	fmt.Fprintf(os.Stderr, "nen-daemon: running (PID %d)\n", os.Getpid())

	statsTicker := time.NewTicker(10 * time.Second)
	defer statsTicker.Stop()

	for {
		select {
		case mode := <-connModeCh:
			stats.ConnectionMode = mode

		case ev, ok := <-ch:
			if !ok {
				writeStats(&stats)
				return nil
			}
			stats.TotalEvents++
			now := ev.Timestamp
			if now.IsZero() {
				now = time.Now().UTC()
			}
			stats.LastEventAt = &now
			routeEvent(ctx, ev, scanners, st, &stats)

		case <-statsTicker.C:
			writeStats(&stats)

		case <-ctx.Done():
			writeStats(&stats)
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

// formatDuration formats a duration as "Xh Ym Zs", omitting leading zero units.
func formatDuration(d time.Duration) string {
	d = d.Round(time.Second)
	h := int(d.Hours())
	m := int(d.Minutes()) % 60
	s := int(d.Seconds()) % 60
	switch {
	case h > 0:
		return fmt.Sprintf("%dh %dm %ds", h, m, s)
	case m > 0:
		return fmt.Sprintf("%dm %ds", m, s)
	default:
		return fmt.Sprintf("%ds", s)
	}
}

func cmdStatus(jsonOutput bool) error {
	pid, running := isRunning()

	type jsonStatus struct {
		Running        bool                    `json:"running"`
		PID            int                     `json:"pid,omitempty"`
		UptimeSeconds  float64                 `json:"uptime_seconds,omitempty"`
		ConnectionMode string                  `json:"connection_mode,omitempty"`
		TotalEvents    int64                   `json:"total_events,omitempty"`
		LastEventAt    *time.Time              `json:"last_event_at,omitempty"`
		ActiveFindings int                     `json:"active_findings,omitempty"`
		Scanners       map[string]*scannerStat `json:"scanners,omitempty"`
	}

	if !running {
		if jsonOutput {
			enc := json.NewEncoder(os.Stdout)
			enc.SetIndent("", "  ")
			return enc.Encode(jsonStatus{Running: false})
		}
		fmt.Println("nen-daemon: not running")
		return nil
	}

	stats, statsErr := readStats()

	// Query active findings count from DB (best-effort).
	activeFindings := -1
	if st, err := openStore(); err == nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		if n, err := st.countActive(ctx); err == nil {
			activeFindings = n
		}
		cancel()
		st.close()
	}

	if jsonOutput {
		out := jsonStatus{
			Running:        true,
			PID:            pid,
			ActiveFindings: activeFindings,
		}
		if statsErr == nil {
			uptime := time.Since(stats.StartedAt)
			out.UptimeSeconds = uptime.Seconds()
			out.ConnectionMode = stats.ConnectionMode
			out.TotalEvents = stats.TotalEvents
			out.LastEventAt = stats.LastEventAt
			out.Scanners = stats.Scanners
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	// Human-readable output.
	fmt.Printf("nen-daemon: running (PID %d)\n", pid)

	if statsErr == nil {
		uptime := time.Since(stats.StartedAt)
		fmt.Printf("%-20s %s\n", "Uptime:", formatDuration(uptime))
		fmt.Printf("%-20s %s\n", "Connection mode:", stats.ConnectionMode)
		fmt.Printf("%-20s %d\n", "Total events:", stats.TotalEvents)
		if stats.LastEventAt != nil {
			fmt.Printf("%-20s %s\n", "Last event:", stats.LastEventAt.Format(time.RFC3339))
		} else {
			fmt.Printf("%-20s %s\n", "Last event:", "-")
		}
	}

	if activeFindings >= 0 {
		fmt.Printf("%-20s %d\n", "Active findings:", activeFindings)
	}

	if statsErr == nil && len(stats.Scanners) > 0 {
		fmt.Println()
		const sep = "------------------------  --------  --------  ------------------------------"
		fmt.Printf("%-24s %8s %8s  %s\n", "Scanner", "Events", "Errors", "Last Error")
		fmt.Println(sep)

		names := make([]string, 0, len(stats.Scanners))
		for name := range stats.Scanners {
			names = append(names, name)
		}
		sort.Strings(names)

		for _, name := range names {
			ss := stats.Scanners[name]
			lastErr := "-"
			if ss.LastError != "" {
				lastErr = ss.LastError
				if len(lastErr) > 40 {
					lastErr = lastErr[:37] + "..."
				}
			}
			fmt.Printf("%-24s %8d %8d  %s\n", name, ss.EventsRouted, ss.ErrorCount, lastErr)
		}
	}

	return nil
}

func main() {
	args := os.Args[1:]
	if len(args) == 0 {
		fmt.Fprintf(os.Stderr, "Usage: nen-daemon <start|stop|status> [--json]\n")
		os.Exit(1)
	}

	var err error
	switch args[0] {
	case "start":
		err = cmdStart()
	case "stop":
		err = cmdStop()
	case "status":
		jsonOutput := false
		for _, a := range args[1:] {
			if a == "--json" {
				jsonOutput = true
			}
		}
		err = cmdStatus(jsonOutput)
	default:
		fmt.Fprintf(os.Stderr, "nen-daemon: unknown command %q\nUsage: nen-daemon <start|stop|status> [--json]\n", args[0])
		os.Exit(1)
	}

	if err != nil {
		fmt.Fprintf(os.Stderr, "nen-daemon %s: %v\n", args[0], err)
		os.Exit(1)
	}
}
