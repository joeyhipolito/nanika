package cron

import (
	"strings"
	"testing"
	"time"
)

// TestNextRunAfter tests the NextRunAfter function with various cron expressions.
func TestNextRunAfter(t *testing.T) {
	tests := []struct {
		name    string
		schedule string
		after   time.Time
		wantErr string
		check   func(t *testing.T, got time.Time, after time.Time)
	}{
		{
			name:     "daily at 9 AM",
			schedule: "0 9 * * *",
			after:    time.Date(2026, 3, 31, 8, 0, 0, 0, time.UTC),
			check: func(t *testing.T, got time.Time, after time.Time) {
				if got.Hour() != 9 || got.Minute() != 0 {
					t.Errorf("got %02d:%02d, want 09:00", got.Hour(), got.Minute())
				}
				if !got.After(after) {
					t.Errorf("result %v should be after %v", got, after)
				}
			},
		},
		{
			name:     "every 5 minutes",
			schedule: "*/5 * * * *",
			after:    time.Date(2026, 3, 31, 8, 12, 30, 0, time.UTC),
			check: func(t *testing.T, got time.Time, after time.Time) {
				// Should be at the next 5-minute boundary
				if got.Minute()%5 != 0 {
					t.Errorf("minute %d not divisible by 5", got.Minute())
				}
				if !got.After(after) {
					t.Errorf("result %v should be after %v", got, after)
				}
			},
		},
		{
			name:     "weekday at 9 AM",
			schedule: "0 9 * * 1-5",
			after:    time.Date(2026, 3, 27, 8, 0, 0, 0, time.UTC), // Friday
			check: func(t *testing.T, got time.Time, after time.Time) {
				// Friday should run
				if got.Weekday() < time.Monday || got.Weekday() > time.Friday {
					t.Errorf("got %v, want weekday (Mon-Fri)", got.Weekday())
				}
			},
		},
		{
			name:     "first day of month",
			schedule: "0 0 1 * *",
			after:    time.Date(2026, 3, 15, 0, 0, 0, 0, time.UTC),
			check: func(t *testing.T, got time.Time, after time.Time) {
				if got.Day() != 1 {
					t.Errorf("got day %d, want 1", got.Day())
				}
				// Should be in April (next month)
				if got.Month() != time.April {
					t.Errorf("got month %v, want April", got.Month())
				}
			},
		},
		{
			name:     "specific day and hour",
			schedule: "30 14 * * *",
			after:    time.Date(2026, 3, 31, 14, 0, 0, 0, time.UTC),
			check: func(t *testing.T, got time.Time, after time.Time) {
				if got.Hour() != 14 || got.Minute() != 30 {
					t.Errorf("got %02d:%02d, want 14:30", got.Hour(), got.Minute())
				}
			},
		},
		{
			name:    "invalid cron expression - too few fields",
			schedule: "0 9 *",
			wantErr: "parsing cron",
		},
		{
			name:    "invalid cron expression - invalid field",
			schedule: "0 25 * * *",
			wantErr: "parsing cron",
		},
		{
			name:    "invalid cron expression - non-numeric",
			schedule: "abc def ghi jkl mno",
			wantErr: "parsing cron",
		},
		{
			name:     "every hour",
			schedule: "0 * * * *",
			after:    time.Date(2026, 3, 31, 14, 30, 0, 0, time.UTC),
			check: func(t *testing.T, got time.Time, after time.Time) {
				if got.Minute() != 0 {
					t.Errorf("got minute %d, want 0", got.Minute())
				}
			},
		},
		{
			name:     "every 15 minutes",
			schedule: "*/15 * * * *",
			after:    time.Date(2026, 3, 31, 14, 22, 0, 0, time.UTC),
			check: func(t *testing.T, got time.Time, after time.Time) {
				if got.Minute()%15 != 0 {
					t.Errorf("minute %d not divisible by 15", got.Minute())
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := NextRunAfter(tt.schedule, tt.after)
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
				tt.check(t, got, tt.after)
			}
		})
	}
}

// TestNextRun tests the NextRun function (which uses current time).
func TestNextRun(t *testing.T) {
	tests := []struct {
		name     string
		schedule string
		wantErr  string
		check    func(t *testing.T, got time.Time)
	}{
		{
			name:     "daily at 9 AM",
			schedule: "0 9 * * *",
			check: func(t *testing.T, got time.Time) {
				now := time.Now().UTC()
				if got.Before(now) {
					t.Errorf("result %v should be after now %v", got, now)
				}
			},
		},
		{
			name:    "invalid expression",
			schedule: "invalid * * * *",
			wantErr: "parsing cron",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := NextRun(tt.schedule)
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

// TestCronRecurrencePattern verifies that multiple calls to NextRunAfter
// produce an increasing sequence of valid times.
func TestCronRecurrencePattern(t *testing.T) {
	schedule := "0 9 * * *" // Daily at 9 AM UTC
	anchor := time.Date(2026, 3, 31, 0, 0, 0, 0, time.UTC)

	var times []time.Time
	current := anchor
	for i := 0; i < 5; i++ {
		next, err := NextRunAfter(schedule, current)
		if err != nil {
			t.Fatalf("iteration %d: %v", i, err)
		}
		times = append(times, next)
		current = next
	}

	// Verify all times are 9 AM
	for i, tm := range times {
		if tm.Hour() != 9 || tm.Minute() != 0 {
			t.Errorf("time %d: got %02d:%02d, want 09:00", i, tm.Hour(), tm.Minute())
		}
	}

	// Verify times are exactly 24 hours apart
	for i := 1; i < len(times); i++ {
		diff := times[i].Sub(times[i-1])
		if diff != 24*time.Hour {
			t.Errorf("diff between times %d and %d: %v, want 24h", i-1, i, diff)
		}
	}
}

// TestCronEdgeCases tests edge cases for cron expressions.
func TestCronEdgeCases(t *testing.T) {
	tests := []struct {
		name     string
		schedule string
		after    time.Time
		check    func(t *testing.T, got time.Time)
	}{
		{
			name:     "leap year February",
			schedule: "0 0 29 2 *",
			after:    time.Date(2024, 1, 1, 0, 0, 0, 0, time.UTC), // 2024 is a leap year
			check: func(t *testing.T, got time.Time) {
				if got.Month() != time.February || got.Day() != 29 {
					t.Errorf("got %v, want Feb 29", got)
				}
			},
		},
		{
			name:     "year boundary",
			schedule: "0 0 1 1 *",
			after:    time.Date(2025, 12, 31, 23, 59, 0, 0, time.UTC),
			check: func(t *testing.T, got time.Time) {
				if got.Year() != 2026 || got.Month() != time.January || got.Day() != 1 {
					t.Errorf("got %v, want 2026-01-01", got)
				}
			},
		},
		{
			name:     "last hour of day",
			schedule: "0 23 * * *",
			after:    time.Date(2026, 3, 31, 22, 0, 0, 0, time.UTC),
			check: func(t *testing.T, got time.Time) {
				if got.Hour() != 23 {
					t.Errorf("got hour %d, want 23", got.Hour())
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := NextRunAfter(tt.schedule, tt.after)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if tt.check != nil {
				tt.check(t, got)
			}
		})
	}
}

// BenchmarkNextRunAfter benchmarks cron schedule calculation.
func BenchmarkNextRunAfter(b *testing.B) {
	schedule := "0 9 * * *"
	after := time.Now().UTC()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, _ = NextRunAfter(schedule, after)
	}
}

// BenchmarkNextRun benchmarks cron schedule calculation with current time.
func BenchmarkNextRun(b *testing.B) {
	schedule := "0 9 * * *"
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, _ = NextRun(schedule)
	}
}
