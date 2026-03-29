package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"text/tabwriter"

	"github.com/spf13/cobra"
)

func newHistoryCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "history",
		Short: "Show recent scheduler job run history",
		RunE: func(cmd *cobra.Command, args []string) error {
			limit, _ := cmd.Flags().GetInt("limit")
			return runHistoryCmd(limit)
		},
	}
	cmd.Flags().Int("limit", 50, "Maximum number of events to show (most recent first)")
	return cmd
}

func runHistoryCmd(limit int) error {
	path := filepath.Join(eventsDir(), "scheduler.jsonl")
	f, err := os.Open(path)
	if os.IsNotExist(err) {
		fmt.Println("no history yet — run the daemon to start recording events")
		return nil
	}
	if err != nil {
		return fmt.Errorf("opening events file: %w", err)
	}
	defer f.Close()

	var events []schedulerEvent
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		var ev schedulerEvent
		if err := json.Unmarshal(line, &ev); err != nil {
			continue // skip malformed lines
		}
		events = append(events, ev)
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("reading events: %w", err)
	}

	if len(events) == 0 {
		fmt.Println("no events recorded yet")
		return nil
	}

	// Trim to the last `limit` entries, then reverse for most-recent-first display.
	if len(events) > limit {
		events = events[len(events)-limit:]
	}
	for i, j := 0, len(events)-1; i < j; i, j = i+1, j-1 {
		events[i], events[j] = events[j], events[i]
	}

	w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
	fmt.Fprintln(w, "TIME\tSTATUS\tJOB\tEXIT\tDURATION\tSTDERR")
	for _, ev := range events {
		status := "ok"
		if ev.Type == "schedule.failed" {
			status = "FAILED"
		}
		dur := fmt.Sprintf("%dms", ev.DurationMs)
		stderr := ev.Stderr
		if len(stderr) > 40 {
			stderr = stderr[:37] + "..."
		}
		fmt.Fprintf(w, "%s\t%s\t%s\t%d\t%s\t%s\n", ev.Ts, status, ev.JobName, ev.ExitCode, dur, stderr)
	}
	return w.Flush()
}
