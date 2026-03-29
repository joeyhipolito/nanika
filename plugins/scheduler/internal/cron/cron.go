// Package cron provides helpers for parsing cron expressions and computing next run times.
package cron

import (
	"fmt"
	"time"

	"github.com/robfig/cron/v3"
)

// defaultParser handles standard 5-field cron expressions (no seconds field).
var defaultParser = cron.NewParser(cron.Minute | cron.Hour | cron.Dom | cron.Month | cron.Dow)

// NextRunAfter returns the next run time for schedule after the given time.
func NextRunAfter(schedule string, after time.Time) (time.Time, error) {
	sched, err := defaultParser.Parse(schedule)
	if err != nil {
		return time.Time{}, fmt.Errorf("parsing cron %q: %w", schedule, err)
	}
	return sched.Next(after), nil
}

// NextRun returns the next run time for schedule after now.
func NextRun(schedule string) (time.Time, error) {
	return NextRunAfter(schedule, time.Now().UTC())
}
