package cmd

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

func init() {
	eventsCmd := &cobra.Command{
		Use:   "events",
		Short: "Inspect mission event logs",
		Long: `Commands for working with mission event logs.

Event logs are written to ~/.alluka/events/<mission-id>.jsonl during execution.
Each line is a JSON-encoded event with a type, timestamp, and optional data.`,
	}

	listCmd := &cobra.Command{
		Use:   "list",
		Short: "List available mission event logs",
		RunE:  runEventsList,
	}

	replayCmd := &cobra.Command{
		Use:   "replay <mission-id>",
		Short: "Print all events from a mission log",
		Long: `Print every event recorded for a mission in sequence order.

The mission-id can be the full ID (e.g. 20260221-c9415db5) or the path
to a .jsonl file directly.`,
		Args: cobra.ExactArgs(1),
		RunE: runEventsReplay,
	}
	replayCmd.Flags().BoolP("json", "j", false, "print raw JSON lines instead of formatted output")

	tailCmd := &cobra.Command{
		Use:   "tail <mission-id>",
		Short: "Stream events from an active mission log (like tail -f)",
		Long: `Follow a mission event log in real time, printing new events as they arrive.

Polls the log file every 100ms. Press Ctrl-C to stop.`,
		Args: cobra.ExactArgs(1),
		RunE: runEventsTail,
	}
	tailCmd.Flags().BoolP("json", "j", false, "print raw JSON lines instead of formatted output")

	eventsCmd.AddCommand(listCmd, replayCmd, tailCmd)
	rootCmd.AddCommand(eventsCmd)
}

// ---- list ---------------------------------------------------------------

func runEventsList(cmd *cobra.Command, args []string) error {
	logsDir, err := event.EventLogsDir()
	if err != nil {
		return fmt.Errorf("resolving events dir: %w", err)
	}

	entries, err := os.ReadDir(logsDir)
	if err != nil {
		if os.IsNotExist(err) {
			fmt.Println("no event logs found (directory does not exist yet)")
			return nil
		}
		return fmt.Errorf("reading events dir: %w", err)
	}

	var logs []os.DirEntry
	for _, e := range entries {
		if !e.IsDir() && strings.HasSuffix(e.Name(), ".jsonl") {
			logs = append(logs, e)
		}
	}

	if len(logs) == 0 {
		fmt.Println("no event logs found")
		return nil
	}

	// Print newest first (entries are sorted lexicographically by name, which
	// are timestamp-prefixed mission IDs — reversing gives newest first).
	fmt.Printf("%-30s  %8s  %s\n", "MISSION ID", "EVENTS", "SIZE")
	fmt.Println(strings.Repeat("-", 55))

	for i := len(logs) - 1; i >= 0; i-- {
		entry := logs[i]
		missionID := strings.TrimSuffix(entry.Name(), ".jsonl")
		path := filepath.Join(logsDir, entry.Name())

		info, err := entry.Info()
		if err != nil {
			continue
		}

		count := countLines(path)
		fmt.Printf("%-30s  %8d  %s\n", missionID, count, humanSize(info.Size()))
	}

	return nil
}

// ---- replay -------------------------------------------------------------

func runEventsReplay(cmd *cobra.Command, args []string) error {
	rawJSON, _ := cmd.Flags().GetBool("json")

	path, err := resolveLogPath(args[0])
	if err != nil {
		return err
	}

	f, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("opening event log: %w", err)
	}
	defer f.Close()

	return scanEvents(f, rawJSON, os.Stdout)
}

// ---- tail ---------------------------------------------------------------

func runEventsTail(cmd *cobra.Command, args []string) error {
	rawJSON, _ := cmd.Flags().GetBool("json")

	path, err := resolveLogPath(args[0])
	if err != nil {
		return err
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigCh
		cancel()
	}()

	// Open and seek to end — tail only shows new events.
	// If the file doesn't exist yet, wait up to 10s for it to appear.
	f, err := waitForFile(ctx, path, 10*time.Second)
	if err != nil {
		return err
	}
	defer f.Close()

	// Seek to end of existing content.
	if _, err := f.Seek(0, io.SeekEnd); err != nil {
		return fmt.Errorf("seeking to end of log: %w", err)
	}

	fmt.Fprintf(os.Stderr, "tailing %s (Ctrl-C to stop)...\n", filepath.Base(path))

	const pollInterval = 100 * time.Millisecond
	buf := bufio.NewReader(f)

	for {
		select {
		case <-ctx.Done():
			return nil
		default:
		}

		line, err := buf.ReadString('\n')
		if len(line) > 0 {
			line = strings.TrimRight(line, "\n")
			if rawJSON {
				fmt.Println(line)
			} else {
				formatEventLine(line, os.Stdout)
			}
			continue // immediately try to read more without sleeping
		}

		if err == io.EOF {
			// No new data — wait a bit then poll again.
			select {
			case <-ctx.Done():
				return nil
			case <-time.After(pollInterval):
			}
			continue
		}

		if err != nil {
			return fmt.Errorf("reading log: %w", err)
		}
	}
}

// ---- helpers ------------------------------------------------------------

// resolveLogPath accepts either a mission ID or a direct .jsonl file path.
func resolveLogPath(arg string) (string, error) {
	// Direct file path
	if strings.HasSuffix(arg, ".jsonl") {
		if _, err := os.Stat(arg); err == nil {
			return arg, nil
		}
	}

	// Mission ID → standard location
	path, err := event.EventLogPath(arg)
	if err != nil {
		return "", fmt.Errorf("resolving log path: %w", err)
	}

	if _, err := os.Stat(path); os.IsNotExist(err) {
		return "", fmt.Errorf("no event log found for mission %q (looked at %s)", arg, path)
	}

	return path, nil
}

// waitForFile polls until the file at path appears or ctx is cancelled.
func waitForFile(ctx context.Context, path string, timeout time.Duration) (*os.File, error) {
	deadline := time.Now().Add(timeout)
	for {
		f, err := os.Open(path)
		if err == nil {
			return f, nil
		}
		if !os.IsNotExist(err) {
			return nil, fmt.Errorf("opening log file: %w", err)
		}
		if time.Now().After(deadline) {
			return nil, fmt.Errorf("log file %s did not appear within %s", path, timeout)
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(200 * time.Millisecond):
		}
	}
}

// scanEvents reads all JSONL events from r and prints them to w.
func scanEvents(r io.Reader, rawJSON bool, w io.Writer) error {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 64*1024), 64*1024) // 64 KiB max line

	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		if rawJSON {
			fmt.Fprintln(w, line)
		} else {
			formatEventLine(line, w)
		}
	}

	return sc.Err()
}

// formatEventLine pretty-prints a single JSONL event line.
func formatEventLine(line string, w io.Writer) {
	var ev event.Event
	if err := json.Unmarshal([]byte(line), &ev); err != nil {
		// Unrecognised line — print as-is
		fmt.Fprintln(w, line)
		return
	}

	ts := ev.Timestamp.Local().Format("15:04:05.000")
	seq := fmt.Sprintf("%4d", ev.Sequence)

	// Colour-code by event category using ANSI codes (gracefully degrades in non-TTY).
	colour := eventColour(ev.Type)
	reset := "\033[0m"

	// Build context suffix
	var parts []string
	if ev.PhaseID != "" {
		parts = append(parts, "phase="+ev.PhaseID)
	}
	if ev.WorkerID != "" {
		parts = append(parts, "worker="+ev.WorkerID)
	}
	for k, v := range ev.Data {
		parts = append(parts, fmt.Sprintf("%s=%v", k, v))
	}

	ctx := ""
	if len(parts) > 0 {
		ctx = "  " + strings.Join(parts, " ")
	}

	fmt.Fprintf(w, "%s %s %s%-30s%s%s\n", ts, seq, colour, ev.Type, reset, ctx)
}

// eventColour returns an ANSI colour prefix for a given event type category.
func eventColour(typ event.EventType) string {
	switch {
	case strings.HasPrefix(string(typ), "mission."):
		return "\033[1;36m" // bold cyan
	case strings.HasPrefix(string(typ), "phase."):
		return "\033[1;34m" // bold blue
	case strings.HasPrefix(string(typ), "worker."):
		return "\033[1;32m" // bold green
	case strings.HasPrefix(string(typ), "system."):
		return "\033[1;33m" // bold yellow
	case strings.HasPrefix(string(typ), "dag."):
		return "\033[35m" // magenta
	default:
		return "\033[0m" // reset
	}
}

// countLines counts non-empty lines in a file (cheap event count approximation).
func countLines(path string) int {
	f, err := os.Open(path)
	if err != nil {
		return 0
	}
	defer f.Close()

	n := 0
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		if sc.Text() != "" {
			n++
		}
	}
	return n
}

// humanSize formats bytes as a human-readable string.
func humanSize(b int64) string {
	switch {
	case b >= 1<<20:
		return fmt.Sprintf("%.1f MB", float64(b)/(1<<20))
	case b >= 1<<10:
		return fmt.Sprintf("%.1f KB", float64(b)/(1<<10))
	default:
		return fmt.Sprintf("%d B", b)
	}
}
