package cmd

import (
	"bufio"
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

// scheduleCommitRun cleans up stale engage-commit-* jobs for yesterday and today,
// then schedules a new one-shot engage-commit-YYYYMMDD job for tomorrow at a
// random time between 08:00 and 20:00.
func scheduleCommitRun() error {
	now := time.Now()
	tomorrow := now.AddDate(0, 0, 1).Format("20060102")
	today := now.Format("20060102")
	yesterday := now.AddDate(0, 0, -1).Format("20060102")

	staleNames := []string{
		"engage-commit-" + yesterday,
		"engage-commit-" + today,
	}
	if err := removeJobsByName(staleNames); err != nil {
		fmt.Fprintf(os.Stderr, "warn: cleaning old commit jobs: %v\n", err)
	}

	name := "engage-commit-" + tomorrow
	cmd := exec.Command("scheduler", "jobs", "add",
		"--name", name,
		"--random-daily", "8:00-20:00",
		"--command", "engage commit --count 3 --reschedule",
	)
	output, err := cmd.CombinedOutput()
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s", output)
		return fmt.Errorf("scheduling %s: %w", name, err)
	}
	fmt.Printf("scheduled: %s (tomorrow, random 08:00–20:00)\n", name)
	return nil
}

// removeJobsByName parses `scheduler jobs` table output and removes jobs
// whose name matches any entry in names.
func removeJobsByName(names []string) error {
	nameSet := make(map[string]struct{}, len(names))
	for _, n := range names {
		nameSet[n] = struct{}{}
	}

	out, err := exec.Command("scheduler", "jobs").CombinedOutput()
	if err != nil {
		return fmt.Errorf("listing jobs: %w", err)
	}

	scanner := bufio.NewScanner(bytes.NewReader(out))
	for scanner.Scan() {
		line := scanner.Text()
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		id, name := fields[0], fields[1]
		// Skip header row.
		if id == "ID" {
			continue
		}
		if _, ok := nameSet[name]; ok {
			rmOut, rmErr := exec.Command("scheduler", "jobs", "remove", id).CombinedOutput()
			if rmErr != nil {
				fmt.Fprintf(os.Stderr, "warn: removing job %s (%s): %v\n%s\n", id, name, rmErr, rmOut)
			}
		}
	}
	return scanner.Err()
}
