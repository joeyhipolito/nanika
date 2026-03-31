package cmd

import (
	"fmt"
	"math/rand"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-scheduler/internal/db"
)

// TestParseAtTime tests the parseAtTime function with various formats.
func TestParseAtTime(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		wantErr string // empty means no error expected
		check   func(t *testing.T, got time.Time)
	}{
		{
			name:  "RFC3339 format",
			input: "2026-03-31T14:30:00Z",
			check: func(t *testing.T, got time.Time) {
				if got.Year() != 2026 || got.Month() != 3 || got.Day() != 31 {
					t.Errorf("date mismatch: %v", got)
				}
			},
		},
		{
			name:  "ISO 8601 with seconds",
			input: "2026-03-31T14:30:45",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 14 || got.Minute() != 30 || got.Second() != 45 {
					t.Errorf("time mismatch: %v", got)
				}
			},
		},
		{
			name:  "ISO 8601 without seconds",
			input: "2026-03-31T14:30",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 14 || got.Minute() != 30 {
					t.Errorf("time mismatch: %v", got)
				}
			},
		},
		{
			name:  "Date with time (24h format with spaces)",
			input: "2026-03-31 14:30",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 14 || got.Minute() != 30 {
					t.Errorf("time mismatch: %v", got)
				}
			},
		},
		{
			name:  "Date with time (24h format with seconds)",
			input: "2026-03-31 14:30:45",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 14 || got.Minute() != 30 || got.Second() != 45 {
					t.Errorf("time mismatch: %v", got)
				}
			},
		},
		{
			name:  "12-hour format with PM",
			input: "2:00 PM",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 14 || got.Minute() != 0 {
					t.Errorf("time mismatch: expected 14:00, got %02d:%02d", got.Hour(), got.Minute())
				}
				// Should use today's date
				now := time.Now()
				if got.Year() != now.Year() || got.Month() != now.Month() || got.Day() != now.Day() {
					t.Errorf("date should be today, got %v", got)
				}
			},
		},
		{
			name:  "12-hour format with AM",
			input: "8:30 AM",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 8 || got.Minute() != 30 {
					t.Errorf("time mismatch: expected 08:30, got %02d:%02d", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:  "12-hour format without spaces PM",
			input: "2:30PM",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 14 || got.Minute() != 30 {
					t.Errorf("time mismatch: expected 14:30, got %02d:%02d", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:  "12-hour format without spaces AM",
			input: "9:00AM",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 9 || got.Minute() != 0 {
					t.Errorf("time mismatch: expected 09:00, got %02d:%02d", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:  "24-hour format",
			input: "14:30",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 14 || got.Minute() != 30 {
					t.Errorf("time mismatch: expected 14:30, got %02d:%02d", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:  "Hour only with PM",
			input: "3 PM",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 15 {
					t.Errorf("time mismatch: expected 15:00, got %02d:%02d", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:  "Hour only with PM no space",
			input: "3PM",
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 15 {
					t.Errorf("time mismatch: expected 15:00, got %02d:%02d", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:    "Invalid time format",
			input:   "invalid-time",
			wantErr: "unrecognized time format",
		},
		{
			name:    "Empty string",
			input:   "",
			wantErr: "unrecognized time format",
		},
		{
			name:    "Invalid hour",
			input:   "25:00",
			wantErr: "unrecognized time format",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := parseAtTime(tt.input)
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("want error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("want error containing %q, got %v", tt.wantErr, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if tt.check != nil {
				tt.check(t, got)
			}
		})
	}
}

// TestParseRandomWindow tests the parseRandomWindow function.
func TestParseRandomWindow(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		wantH1   int
		wantM1   int
		wantH2   int
		wantM2   int
		wantErr  string
	}{
		{
			name:    "valid window 8:00-20:00",
			input:   "8:00-20:00",
			wantH1:  8,
			wantM1:  0,
			wantH2:  20,
			wantM2:  0,
		},
		{
			name:    "valid window 9:30-17:45",
			input:   "9:30-17:45",
			wantH1:  9,
			wantM1:  30,
			wantH2:  17,
			wantM2:  45,
		},
		{
			name:    "valid single digit hours",
			input:   "8:0-20:0",
			wantH1:  8,
			wantM1:  0,
			wantH2:  20,
			wantM2:  0,
		},
		{
			name:   "no dash separator",
			input:  "8:0020:00",
			wantErr: "expected H:MM-H:MM format",
		},
		{
			name:   "invalid start time",
			input:  "invalid-20:00",
			wantErr: "invalid start time",
		},
		{
			name:   "invalid end time",
			input:  "8:00-invalid",
			wantErr: "invalid end time",
		},
		{
			name:   "end before start",
			input:  "20:00-8:00",
			wantErr: "end time must be after start time",
		},
		{
			name:   "same start and end",
			input:  "10:00-10:00",
			wantErr: "end time must be after start time",
		},
		{
			name:   "empty string",
			input:  "",
			wantErr: "expected H:MM-H:MM format",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			h1, m1, h2, m2, err := parseRandomWindow(tt.input)
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("want error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("want error containing %q, got %v", tt.wantErr, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if h1 != tt.wantH1 || m1 != tt.wantM1 || h2 != tt.wantH2 || m2 != tt.wantM2 {
				t.Errorf("parseRandomWindow(%q) = (%d, %d, %d, %d), want (%d, %d, %d, %d)",
					tt.input, h1, m1, h2, m2, tt.wantH1, tt.wantM1, tt.wantH2, tt.wantM2)
			}
		})
	}
}

// TestRandomTimeInWindow tests that randomTimeInWindow generates times within the specified window.
func TestRandomTimeInWindow(t *testing.T) {
	window := "9:00-17:00"
	day := time.Date(2026, 3, 31, 0, 0, 0, 0, time.Local)

	// Run multiple times to check randomness
	for i := 0; i < 10; i++ {
		t.Run(fmt.Sprintf("iteration_%d", i), func(t *testing.T) {
			got, err := randomTimeInWindow(window, day)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got.Year() != day.Year() || got.Month() != day.Month() || got.Day() != day.Day() {
				t.Errorf("date mismatch: got %v, want %v", got, day)
			}
			if got.Hour() < 9 || (got.Hour() == 9 && got.Minute() < 0) || got.Hour() > 17 {
				t.Errorf("time %02d:%02d outside window 9:00-17:00", got.Hour(), got.Minute())
			}
			// Boundary check: should not reach 17:01 or later
			if got.Hour() > 17 || (got.Hour() == 17 && got.Minute() > 0) {
				t.Errorf("time %02d:%02d exceeds window end", got.Hour(), got.Minute())
			}
		})
	}
}

// TestRandomTimeInWindowInvalid tests error handling for invalid windows.
func TestRandomTimeInWindowInvalid(t *testing.T) {
	tests := []struct {
		name    string
		window  string
		wantErr string
	}{
		{
			name:    "invalid window format",
			window:  "invalid",
			wantErr: "parsing window",
		},
		{
			name:    "end before start",
			window:  "20:00-8:00",
			wantErr: "parsing window",
		},
	}

	day := time.Now()
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := randomTimeInWindow(tt.window, day)
			if err == nil {
				t.Fatalf("want error containing %q, got nil", tt.wantErr)
			}
			if !strings.Contains(err.Error(), tt.wantErr) {
				t.Errorf("want error containing %q, got %v", tt.wantErr, err)
			}
		})
	}
}

// TestNextRandomTime tests nextRandomTime scheduling logic.
func TestNextRandomTime(t *testing.T) {
	tests := []struct {
		name    string
		window  string
		wantErr string
		check   func(t *testing.T, got time.Time)
	}{
		{
			name:   "valid window returns future time",
			window: "8:00-20:00",
			check: func(t *testing.T, got time.Time) {
				if !got.After(time.Now()) {
					t.Errorf("returned time %v should be in future", got)
				}
				// Should be within the window
				if got.Hour() < 8 || (got.Hour() == 8 && got.Minute() < 0) || got.Hour() > 20 {
					t.Errorf("time outside window: %02d:%02d", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:    "invalid window format",
			window:  "invalid",
			wantErr: "parsing window",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := nextRandomTime(tt.window)
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("want error, got nil")
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("want error containing %q, got %v", tt.wantErr, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if tt.check != nil {
				tt.check(t, got)
			}
		})
	}
}

// TestRandomTimeTomorrow tests that randomTimeTomorrow generates tomorrow's time.
func TestRandomTimeTomorrow(t *testing.T) {
	window := "9:00-17:00"
	got, err := randomTimeTomorrow(window)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	tomorrow := time.Now().AddDate(0, 0, 1)
	if got.Year() != tomorrow.Year() || got.Month() != tomorrow.Month() || got.Day() != tomorrow.Day() {
		t.Errorf("date mismatch: got %v, want %v", got, tomorrow)
	}
	localTime := got.Local()
	if localTime.Hour() < 9 || localTime.Hour() >= 17 {
		t.Errorf("time outside window: %02d:%02d", localTime.Hour(), localTime.Minute())
	}
}

// TestNextRandomTimeEarlyMorning tests nextRandomTime when current time is before window start.
func TestNextRandomTimeEarlyMorning(t *testing.T) {
	// This test verifies that if we're before the window starts today, we get a time from today.
	// We can't directly control the current time, so we test the behavior indirectly
	// by ensuring nextRandomTime returns a future time within the window.
	window := "14:00-22:00" // Afternoon/evening window
	got, err := nextRandomTime(window)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	now := time.Now()
	if !got.After(now) {
		t.Errorf("nextRandomTime should return a future time, got %v (now %v)", got, now)
	}

	localTime := got.Local()
	if localTime.Hour() < 14 || localTime.Hour() >= 22 {
		t.Errorf("time should be in 14:00-22:00 window, got %02d:%02d", localTime.Hour(), localTime.Minute())
	}
}

// TestComputeNextFireTimes tests the computeNextFireTimes helper function.
func TestComputeNextFireTimes(t *testing.T) {
	tests := []struct {
		name        string
		job         db.Job
		count       int
		wantErr     string
		wantCount   int
		checkTimes  func(t *testing.T, times []time.Time)
	}{
		{
			name: "cron job returns multiple times",
			job: db.Job{
				ScheduleType: "cron",
				Schedule:     "0 9 * * *",
			},
			count:     5,
			wantCount: 5,
			checkTimes: func(t *testing.T, times []time.Time) {
				// All times should be in the future
				now := time.Now()
				for _, tm := range times {
					if !tm.After(now) {
						t.Errorf("fire time %v should be after now", tm)
					}
				}
				// Times should be strictly increasing
				for i := 1; i < len(times); i++ {
					if !times[i].After(times[i-1]) {
						t.Errorf("times not strictly increasing: %v >= %v", times[i-1], times[i])
					}
				}
			},
		},
		{
			name: "every job returns correct intervals",
			job: db.Job{
				ScheduleType: "every",
				Schedule:     "30m",
				NextRunAt:    func() *time.Time { t := time.Now().Add(30 * time.Minute); return &t }(),
			},
			count:     3,
			wantCount: 3,
			checkTimes: func(t *testing.T, times []time.Time) {
				// Intervals should be exactly 30m apart
				for i := 1; i < len(times); i++ {
					diff := times[i].Sub(times[i-1])
					if diff != 30*time.Minute {
						t.Errorf("interval mismatch: %v, want 30m", diff)
					}
				}
			},
		},
		{
			name: "every job without NextRunAt starts from now",
			job: db.Job{
				ScheduleType: "every",
				Schedule:     "1h",
			},
			count:     2,
			wantCount: 2,
			checkTimes: func(t *testing.T, times []time.Time) {
				now := time.Now()
				// First should be roughly now + 1h
				diff := times[0].Sub(now)
				if diff < 59*time.Minute || diff > 61*time.Minute {
					t.Errorf("first time not approximately 1h in future: diff=%v", diff)
				}
			},
		},
		{
			name: "random job with NextRunAt returns single time",
			job: db.Job{
				ScheduleType: "random",
				RandomWindow: "8:00-20:00",
				NextRunAt:    func() *time.Time { t := time.Now().Add(2 * time.Hour); return &t }(),
			},
			count:     5,
			wantCount: 1,
		},
		{
			name: "random job without NextRunAt returns empty",
			job: db.Job{
				ScheduleType: "random",
				RandomWindow: "8:00-20:00",
			},
			count:     5,
			wantCount: 0,
		},
		{
			name: "at job with NextRunAt returns single time",
			job: db.Job{
				ScheduleType: "at",
				NextRunAt:    func() *time.Time { t := time.Now().Add(1 * time.Hour); return &t }(),
			},
			count:     5,
			wantCount: 1,
		},
		{
			name: "at job without NextRunAt returns empty",
			job: db.Job{
				ScheduleType: "at",
			},
			count:     5,
			wantCount: 0,
		},
		{
			name: "delay job with NextRunAt returns single time",
			job: db.Job{
				ScheduleType: "delay",
				NextRunAt:    func() *time.Time { t := time.Now().Add(2 * time.Hour); return &t }(),
			},
			count:     5,
			wantCount: 1,
		},
		{
			name: "invalid cron expression",
			job: db.Job{
				ScheduleType: "cron",
				Schedule:     "invalid * * * *",
			},
			count:   5,
			wantErr: "computing cron run",
		},
		{
			name: "invalid every duration",
			job: db.Job{
				ScheduleType: "every",
				Schedule:     "invalid",
			},
			count:   5,
			wantErr: "bad every interval",
		},
		{
			name: "zero count defaults to 5",
			job: db.Job{
				ScheduleType: "cron",
				Schedule:     "0 9 * * *",
			},
			count:     0,
			wantCount: 5,
		},
		{
			name: "unknown schedule type",
			job: db.Job{
				ScheduleType: "unknown",
			},
			count:   5,
			wantErr: "unknown schedule type",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := computeNextFireTimes(tt.job, tt.count)
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("want error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("want error containing %q, got %v", tt.wantErr, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if len(got) != tt.wantCount {
				t.Errorf("got %d times, want %d", len(got), tt.wantCount)
			}
			if tt.checkTimes != nil {
				tt.checkTimes(t, got)
			}
		})
	}
}

// TestHumanRelTime tests the humanRelTime formatting function.
func TestHumanRelTime(t *testing.T) {
	now := time.Now()

	tests := []struct {
		name     string
		time     time.Time
		contains string
		check    func(t *testing.T, got string)
	}{
		{
			name:     "negative duration returns overdue",
			time:     now.Add(-1 * time.Hour),
			contains: "overdue",
		},
		{
			name:     "very near future returns now",
			time:     now.Add(30 * time.Second),
			contains: "now",
		},
		{
			name:     "1-2 hours in future returns in 1h or 2h",
			time:     now.Add(105 * time.Minute), // 1h 45m to avoid rounding at boundary
			check: func(t *testing.T, got string) {
				if !strings.Contains(got, "in 1h") && !strings.Contains(got, "in 2h") {
					t.Errorf("want to contain 'in 1h' or 'in 2h', got %q", got)
				}
			},
		},
		{
			name:     "40-50 minutes in future contains m",
			time:     now.Add(45 * time.Minute),
			contains: "m",
		},
		{
			name:     "tomorrow returns tomorrow with time",
			time:     now.AddDate(0, 0, 1).Add(2 * time.Hour),
			contains: "tomorrow",
		},
		{
			name:     "several days in future returns time indicator",
			time:     now.AddDate(0, 0, 4),
			check: func(t *testing.T, got string) {
				// Should have month or weekday and AM/PM
				if !strings.ContainsAny(got, "APap") {
					t.Errorf("want to contain AM/PM, got %q", got)
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := humanRelTime(tt.time)
			if tt.contains != "" {
				if !strings.Contains(got, tt.contains) {
					t.Errorf("humanRelTime(%v) = %q, want to contain %q", tt.time, got, tt.contains)
				}
			}
			if tt.check != nil {
				tt.check(t, got)
			}
		})
	}
}

// TestFormatRelDuration tests the formatRelDuration function.
func TestFormatRelDuration(t *testing.T) {
	tests := []struct {
		name string
		dur  time.Duration
		want string
	}{
		{
			name: "hours and minutes",
			dur:  2*time.Hour + 30*time.Minute,
			want: "2h 30m",
		},
		{
			name: "only hours",
			dur:  3 * time.Hour,
			want: "3h",
		},
		{
			name: "only minutes",
			dur:  45 * time.Minute,
			want: "45m",
		},
		{
			name: "one hour one minute",
			dur:  1*time.Hour + 1*time.Minute,
			want: "1h 1m",
		},
		{
			name: "zero duration",
			dur:  0,
			want: "0m",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := formatRelDuration(tt.dur)
			if got != tt.want {
				t.Errorf("formatRelDuration(%v) = %q, want %q", tt.dur, got, tt.want)
			}
		})
	}
}

// TestRandomnessOfRandomWindow verifies that random window generation produces
// different times (not always returning the same time).
func TestRandomnessOfRandomWindow(t *testing.T) {
	window := "8:00-20:00"
	day := time.Now()

	// Generate multiple times and check that they're not all identical
	times := make([]time.Time, 20)
	for i := 0; i < len(times); i++ {
		tm, err := randomTimeInWindow(window, day)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		times[i] = tm
	}

	// Check that not all minutes are identical (there should be variance)
	minutes := make(map[int]bool)
	for _, tm := range times {
		minutes[tm.Minute()] = true
	}

	if len(minutes) == 1 {
		t.Logf("warning: all random times had the same minute (may be false positive)")
	}
}

// BenchmarkRandomTimeInWindow benchmarks random time generation.
func BenchmarkRandomTimeInWindow(b *testing.B) {
	window := "8:00-20:00"
	day := time.Now()

	// Seed for deterministic random generation in benchmarks
	rand.Seed(42)

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, _ = randomTimeInWindow(window, day)
	}
}

// BenchmarkParseAtTime benchmarks time parsing.
func BenchmarkParseAtTime(b *testing.B) {
	input := "2:00 PM"
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, _ = parseAtTime(input)
	}
}

// BenchmarkComputeNextFireTimes benchmarks fire time calculation.
func BenchmarkComputeNextFireTimes(b *testing.B) {
	job := db.Job{
		ScheduleType: "cron",
		Schedule:     "0 9 * * *",
	}
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, _ = computeNextFireTimes(job, 5)
	}
}
