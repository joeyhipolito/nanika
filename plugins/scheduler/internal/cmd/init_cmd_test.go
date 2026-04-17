package cmd

import (
	"strings"
	"testing"

	cronutil "github.com/joeyhipolito/nanika-scheduler/internal/cron"
)

// TestDefaultJobs_IsEmpty asserts that scheduler ships with no cross-plugin
// default jobs. The scheduler plugin owns execution infrastructure only; jobs
// that reference commands from other plugins (scout, engage, shu, ko) belong
// to those plugins' own init commands. Adding a job here for a plugin that
// may not be installed creates a leaky dependency and causes silent nightly
// failures on installs that curate the plugin set.
func TestDefaultJobs_IsEmpty(t *testing.T) {
	if len(defaultJobs) != 0 {
		var names []string
		for _, j := range defaultJobs {
			names = append(names, j.name)
		}
		t.Fatalf("defaultJobs must stay empty (plugin ownership rule). Found: %s. Move cross-plugin jobs to their owning plugin's init command.", strings.Join(names, ", "))
	}
}

// TestDefaultJobs_HaveValidCron is a forward-compatibility guard: if some
// future refactor genuinely does add scheduler-owned jobs here (jobs that
// only reference scheduler itself — e.g. a log rotation job), each entry
// must still have a parseable cron expression. Currently a no-op when the
// slice is empty.
func TestDefaultJobs_HaveValidCron(t *testing.T) {
	for _, j := range defaultJobs {
		j := j
		t.Run(j.name, func(t *testing.T) {
			if _, err := cronutil.NextRun(j.schedule); err != nil {
				t.Errorf("invalid cron %q for job %q: %v", j.schedule, j.name, err)
			}
		})
	}
}

// TestDefaultJobs_HaveNonEmptyCommand — same forward-compatibility guard as
// the cron test. No-op while defaultJobs is empty.
func TestDefaultJobs_HaveNonEmptyCommand(t *testing.T) {
	for _, j := range defaultJobs {
		if strings.TrimSpace(j.command) == "" {
			t.Errorf("job %q has empty command", j.name)
		}
	}
}

// TestDefaultJobs_UniqueNames — same forward-compatibility guard. Catches
// accidental duplicates if the slice ever grows.
func TestDefaultJobs_UniqueNames(t *testing.T) {
	seen := make(map[string]bool, len(defaultJobs))
	for _, j := range defaultJobs {
		if seen[j.name] {
			t.Errorf("duplicate default job name: %q", j.name)
		}
		seen[j.name] = true
	}
}
