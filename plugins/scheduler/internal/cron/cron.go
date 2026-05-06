// Package cron provides helpers for parsing cron expressions and computing next run times.
package cron

import (
	"fmt"
	"regexp"
	"strings"
	"time"

	"github.com/robfig/cron/v3"
)

// defaultParser handles standard 5-field cron expressions (no seconds field).
var defaultParser = cron.NewParser(cron.Minute | cron.Hour | cron.Dom | cron.Month | cron.Dow)

// descriptorParser uses robfig/cron/v3's descriptor format with leading @
var descriptorParser = cron.NewParser(
	cron.SecondOptional | cron.Minute | cron.Hour | cron.Dom | cron.Month | cron.Dow,
)

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

// ParseNaturalLanguage converts natural-language cron expressions to standard 5-field cron format.
// Supports phrases like:
//   - "every 2h" / "every 2 hours"
//   - "daily at 9am" / "daily at 9:00 AM"
//   - "weekly on monday at 6pm" / "weekly on monday at 6:00 PM"
//   - standard 5-field cron expressions (e.g., "0 9 * * *")
//   - descriptors like "@hourly", "@daily", "@weekly"
func ParseNaturalLanguage(input string) (string, error) {
	input = strings.TrimSpace(input)

	// Check if it's already a valid cron expression
	if isValidCron(input) {
		return input, nil
	}

	// Check if it's a descriptor like @hourly, @daily, etc.
	if isValidDescriptor(input) {
		// Convert descriptor to standard cron format
		desc, err := descriptorToStandardCron(input)
		if err == nil {
			return desc, nil
		}
	}

	// Try to parse natural language patterns
	// Pattern: "every N [hours|minutes|days]"
	if every := parseEvery(input); every != "" {
		return every, nil
	}

	// Pattern: "daily at HH:MM [AM|PM]"
	if daily := parseDaily(input); daily != "" {
		return daily, nil
	}

	// Pattern: "weekly on WEEKDAY at HH:MM [AM|PM]"
	if weekly := parseWeekly(input); weekly != "" {
		return weekly, nil
	}

	return "", fmt.Errorf("unable to parse cron expression: %q (try '0 9 * * *', 'daily at 9am', or 'every 2h')", input)
}

// isValidCron checks if the input is a valid 5-field cron expression
func isValidCron(input string) bool {
	_, err := defaultParser.Parse(input)
	return err == nil
}

// isValidDescriptor checks if the input is a recognized cron descriptor
func isValidDescriptor(input string) bool {
	descriptors := map[string]bool{
		"@annually":  true,
		"@yearly":    true,
		"@monthly":   true,
		"@weekly":    true,
		"@daily":     true,
		"@midnight":  true,
		"@hourly":    true,
	}
	lower := strings.ToLower(input)
	return descriptors[lower]
}

// descriptorToStandardCron converts @descriptors to standard 5-field cron
func descriptorToStandardCron(desc string) (string, error) {
	switch strings.ToLower(desc) {
	case "@annually", "@yearly":
		return "0 0 1 1 *", nil
	case "@monthly":
		return "0 0 1 * *", nil
	case "@weekly":
		return "0 0 * * 0", nil
	case "@daily", "@midnight":
		return "0 0 * * *", nil
	case "@hourly":
		return "0 * * * *", nil
	default:
		return "", fmt.Errorf("unknown descriptor: %q", desc)
	}
}

// parseEvery handles patterns like "every 2h", "every 2 hours", "every 30m"
func parseEvery(input string) string {
	re := regexp.MustCompile(`(?i)^every\s+(\d+)\s*([hdm]|hours?|days?|minutes?)$`)
	matches := re.FindStringSubmatch(input)
	if len(matches) != 3 {
		return ""
	}

	num := matches[1]
	unit := strings.ToLower(matches[2])

	// Normalize unit
	if unit == "h" || unit == "hour" || unit == "hours" {
		// every Nh -> 0 */N * * * (at minute 0, every N hours)
		return "0 */" + num + " * * *"
	} else if unit == "m" || unit == "minute" || unit == "minutes" {
		// every Nm -> */N * * * * (every N minutes)
		return "*/" + num + " * * * *"
	} else if unit == "d" || unit == "day" || unit == "days" {
		// every Nd -> 0 0 * * * (at midnight every N days)
		// Note: 5-field cron doesn't directly support "every N days", so we approximate as daily
		// A more precise approach would require checking execution context
		return "0 0 * * *"
	}
	return ""
}

// parseDaily handles patterns like "daily at 9am", "daily at 9:00 AM", "daily at 9"
func parseDaily(input string) string {
	re := regexp.MustCompile(`(?i)^daily\s+at\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?$`)
	matches := re.FindStringSubmatch(input)
	if len(matches) != 4 {
		return ""
	}

	hour := matches[1]
	minute := matches[2]
	ampm := strings.ToLower(matches[3])

	m := 0
	if minute == "" {
		m = 0
	} else {
		if _, err := fmt.Sscanf(minute, "%d", &m); err != nil {
			return ""
		}
	}

	// Convert 12-hour to 24-hour format
	h := 0
	if _, err := fmt.Sscanf(hour, "%d", &h); err != nil {
		return ""
	}

	if ampm == "pm" && h != 12 {
		h += 12
	} else if ampm == "am" && h == 12 {
		h = 0
	}

	return fmt.Sprintf("%d %d * * *", m, h)
}

// parseWeekly handles patterns like "weekly on monday at 9am", "weekly on Mon at 9:00 AM"
func parseWeekly(input string) string {
	re := regexp.MustCompile(`(?i)^weekly\s+on\s+(\w+)\s+at\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?$`)
	matches := re.FindStringSubmatch(input)
	if len(matches) != 5 {
		return ""
	}

	dayName := strings.ToLower(matches[1])
	hour := matches[2]
	minute := matches[3]
	ampm := strings.ToLower(matches[4])

	m := 0
	if minute == "" {
		m = 0
	} else {
		if _, err := fmt.Sscanf(minute, "%d", &m); err != nil {
			return ""
		}
	}

	// Convert day name to number (0=Sunday, 1=Monday, ..., 6=Saturday)
	dayMap := map[string]string{
		"sunday":    "0",
		"sun":       "0",
		"monday":    "1",
		"mon":       "1",
		"tuesday":   "2",
		"tue":       "2",
		"wednesday": "3",
		"wed":       "3",
		"thursday":  "4",
		"thu":       "4",
		"friday":    "5",
		"fri":       "5",
		"saturday":  "6",
		"sat":       "6",
	}

	day, ok := dayMap[dayName]
	if !ok {
		return ""
	}

	// Convert 12-hour to 24-hour format
	h := 0
	if _, err := fmt.Sscanf(hour, "%d", &h); err != nil {
		return ""
	}

	if ampm == "pm" && h != 12 {
		h += 12
	} else if ampm == "am" && h == 12 {
		h = 0
	}

	return fmt.Sprintf("%d %d * * %s", m, h, day)
}
