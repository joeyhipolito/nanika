// Package peak provides peak-hours detection for nanika agents.
//
// Callers load a Config once with LoadConfig, then query it with IsPeak,
// TimeUntilPeakStart, and TimeUntilPeakEnd.  All three accept a Config so the
// caller controls caching.
//
// Default peak window: weekdays 05:00–11:00 America/Los_Angeles, enabled=true.
// Override via ~/.alluka/peak-hours.json; the file is optional.
package peak

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Config holds the peak hours configuration.
type Config struct {
	Enabled   bool   `json:"enabled"`
	StartHour int    `json:"start_hour"` // 24-hour, inclusive; e.g. 5
	EndHour   int    `json:"end_hour"`   // 24-hour, exclusive; e.g. 11
	Timezone  string `json:"timezone"`   // IANA zone, e.g. "America/Los_Angeles"
}

var defaultConfig = Config{
	Enabled:   true,
	StartHour: 5,
	EndHour:   11,
	Timezone:  "America/Los_Angeles",
}

// LoadConfig reads ~/.alluka/peak-hours.json, returning defaults when the file
// is absent.  Returns an error only for malformed JSON or an unrecognised
// timezone value.
func LoadConfig() (Config, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return defaultConfig, fmt.Errorf("getting home directory: %w", err)
	}
	path := filepath.Join(home, ".alluka", "peak-hours.json")
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return defaultConfig, nil
	}
	if err != nil {
		return defaultConfig, fmt.Errorf("reading peak-hours config: %w", err)
	}
	cfg := defaultConfig
	if err := json.Unmarshal(data, &cfg); err != nil {
		return defaultConfig, fmt.Errorf("parsing peak-hours config: %w", err)
	}
	if _, err := time.LoadLocation(cfg.Timezone); err != nil {
		return defaultConfig, fmt.Errorf("invalid timezone %q in peak-hours config: %w", cfg.Timezone, err)
	}
	return cfg, nil
}

// IsPeak reports whether now is within peak hours according to cfg.
// Peak windows are weekdays [StartHour, EndHour) in the configured timezone.
// Returns false if cfg.Enabled is false or the timezone is unrecognised.
func IsPeak(cfg Config) bool {
	if !cfg.Enabled {
		return false
	}
	loc, err := time.LoadLocation(cfg.Timezone)
	if err != nil {
		return false
	}
	return isPeakAt(time.Now().In(loc), cfg)
}

// TimeUntilPeakStart returns the duration from now until the next peak window
// begins.  Returns 0 if currently in peak or cfg.Enabled is false.
func TimeUntilPeakStart(cfg Config) time.Duration {
	if !cfg.Enabled {
		return 0
	}
	loc, err := time.LoadLocation(cfg.Timezone)
	if err != nil {
		return 0
	}
	return timeUntilPeakStart(time.Now().In(loc), cfg)
}

// TimeUntilPeakEnd returns the duration from now until the current peak window
// ends.  Returns 0 if not currently in peak or cfg.Enabled is false.
func TimeUntilPeakEnd(cfg Config) time.Duration {
	if !cfg.Enabled {
		return 0
	}
	loc, err := time.LoadLocation(cfg.Timezone)
	if err != nil {
		return 0
	}
	return timeUntilPeakEnd(time.Now().In(loc), cfg)
}

// isPeakAt is the time-injectable core.  t must already be in the target
// location.  Does NOT check cfg.Enabled (callers handle that).
func isPeakAt(t time.Time, cfg Config) bool {
	if t.Weekday() == time.Saturday || t.Weekday() == time.Sunday {
		return false
	}
	h := t.Hour()
	return h >= cfg.StartHour && h < cfg.EndHour
}

// timeUntilPeakStart returns the duration from t until the next peak start.
// Returns 0 if t is already inside the peak window.  t must be in the target
// location.
func timeUntilPeakStart(t time.Time, cfg Config) time.Duration {
	if isPeakAt(t, cfg) {
		return 0
	}
	// Walk forward at most 8 days (covers the longest possible weekend gap).
	candidate := t
	for range 8 {
		if candidate.Weekday() != time.Saturday && candidate.Weekday() != time.Sunday {
			start := time.Date(
				candidate.Year(), candidate.Month(), candidate.Day(),
				cfg.StartHour, 0, 0, 0, candidate.Location(),
			)
			if start.After(candidate) {
				return start.Sub(t)
			}
		}
		// Advance to midnight of the next calendar day.
		candidate = time.Date(
			candidate.Year(), candidate.Month(), candidate.Day()+1,
			0, 0, 0, 0, candidate.Location(),
		)
	}
	return 0
}

// timeUntilPeakEnd returns the duration from t until the current peak window
// ends.  Returns 0 if t is outside the peak window.  t must be in the target
// location.
func timeUntilPeakEnd(t time.Time, cfg Config) time.Duration {
	if !isPeakAt(t, cfg) {
		return 0
	}
	end := time.Date(
		t.Year(), t.Month(), t.Day(),
		cfg.EndHour, 0, 0, 0, t.Location(),
	)
	return end.Sub(t)
}
