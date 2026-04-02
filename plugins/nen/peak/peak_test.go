package peak

import (
	"os"
	"testing"
	"time"
)

// laLoc is a convenience helper for tests — panics only on bad setup.
func laLoc(t *testing.T) *time.Location {
	t.Helper()
	loc, err := time.LoadLocation("America/Los_Angeles")
	if err != nil {
		t.Fatalf("loading America/Los_Angeles: %v", err)
	}
	return loc
}

var stdCfg = Config{
	Enabled:   true,
	StartHour: 5,
	EndHour:   11,
	Timezone:  "America/Los_Angeles",
}

// ---------------------------------------------------------------------------
// isPeakAt — time correctness
// ---------------------------------------------------------------------------

func TestIsPeakAt_DuringPeak(t *testing.T) {
	loc := laLoc(t)
	// Monday 2026-03-30 07:00 LA
	at := time.Date(2026, 3, 30, 7, 0, 0, 0, loc)
	if !isPeakAt(at, stdCfg) {
		t.Errorf("isPeakAt(%v) = false, want true", at)
	}
}

func TestIsPeakAt_AtStartHourInclusive(t *testing.T) {
	loc := laLoc(t)
	at := time.Date(2026, 3, 30, 5, 0, 0, 0, loc) // exactly StartHour
	if !isPeakAt(at, stdCfg) {
		t.Errorf("isPeakAt at start hour = false, want true")
	}
}

func TestIsPeakAt_AtEndHourExclusive(t *testing.T) {
	loc := laLoc(t)
	at := time.Date(2026, 3, 30, 11, 0, 0, 0, loc) // exactly EndHour — not peak
	if isPeakAt(at, stdCfg) {
		t.Errorf("isPeakAt at end hour = true, want false (exclusive)")
	}
}

func TestIsPeakAt_BeforeStartHour(t *testing.T) {
	loc := laLoc(t)
	at := time.Date(2026, 3, 30, 4, 59, 0, 0, loc)
	if isPeakAt(at, stdCfg) {
		t.Errorf("isPeakAt before start = true, want false")
	}
}

func TestIsPeakAt_AfterEndHour(t *testing.T) {
	loc := laLoc(t)
	at := time.Date(2026, 3, 30, 12, 0, 0, 0, loc)
	if isPeakAt(at, stdCfg) {
		t.Errorf("isPeakAt after end = true, want false")
	}
}

func TestIsPeakAt_Saturday(t *testing.T) {
	loc := laLoc(t)
	at := time.Date(2026, 3, 28, 8, 0, 0, 0, loc) // Saturday
	if isPeakAt(at, stdCfg) {
		t.Errorf("isPeakAt Saturday = true, want false")
	}
}

func TestIsPeakAt_Sunday(t *testing.T) {
	loc := laLoc(t)
	at := time.Date(2026, 3, 29, 8, 0, 0, 0, loc) // Sunday
	if isPeakAt(at, stdCfg) {
		t.Errorf("isPeakAt Sunday = true, want false")
	}
}

// ---------------------------------------------------------------------------
// IsPeak — disabled flag
// ---------------------------------------------------------------------------

func TestIsPeak_Disabled(t *testing.T) {
	cfg := Config{Enabled: false, StartHour: 5, EndHour: 11, Timezone: "America/Los_Angeles"}
	// IsPeak consults time.Now() but must short-circuit on Enabled=false.
	if IsPeak(cfg) {
		t.Error("IsPeak with Enabled=false = true, want false")
	}
}

// ---------------------------------------------------------------------------
// timeUntilPeakStart
// ---------------------------------------------------------------------------

func TestTimeUntilPeakStart_DuringPeak_ReturnsZero(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 30, 8, 0, 0, 0, loc) // Monday 08:00
	if d := timeUntilPeakStart(now, stdCfg); d != 0 {
		t.Errorf("timeUntilPeakStart during peak = %v, want 0", d)
	}
}

func TestTimeUntilPeakStart_BeforePeak_SameDay(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 30, 4, 0, 0, 0, loc) // Monday 04:00 — 1 h before
	want := time.Hour
	if d := timeUntilPeakStart(now, stdCfg); d != want {
		t.Errorf("timeUntilPeakStart = %v, want %v", d, want)
	}
}

func TestTimeUntilPeakStart_AfterPeak_NextWeekday(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 30, 12, 0, 0, 0, loc) // Monday noon — next is Tue 05:00
	want := 17 * time.Hour
	if d := timeUntilPeakStart(now, stdCfg); d != want {
		t.Errorf("timeUntilPeakStart = %v, want %v", d, want)
	}
}

func TestTimeUntilPeakStart_FridayAfterPeak_SkipsWeekend(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 4, 3, 12, 0, 0, 0, loc) // Friday noon
	// Next peak: Monday 2026-04-06 05:00 = Fri noon + 2d17h = 65h
	want := (2*24 + 17) * time.Hour
	if d := timeUntilPeakStart(now, stdCfg); d != want {
		t.Errorf("timeUntilPeakStart friday noon = %v, want %v", d, want)
	}
}

func TestTimeUntilPeakStart_Saturday_SkipsToMonday(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 28, 8, 0, 0, 0, loc) // Saturday 08:00
	// Monday 05:00 = Sat 08:00 + 1d21h = 45h
	want := (1*24 + 21) * time.Hour
	if d := timeUntilPeakStart(now, stdCfg); d != want {
		t.Errorf("timeUntilPeakStart saturday = %v, want %v", d, want)
	}
}

func TestTimeUntilPeakStart_Sunday_SkipsToMonday(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 29, 22, 0, 0, 0, loc) // Sunday 22:00
	// Monday 05:00 = 7h later
	want := 7 * time.Hour
	if d := timeUntilPeakStart(now, stdCfg); d != want {
		t.Errorf("timeUntilPeakStart sunday = %v, want %v", d, want)
	}
}

// ---------------------------------------------------------------------------
// timeUntilPeakEnd
// ---------------------------------------------------------------------------

func TestTimeUntilPeakEnd_DuringPeak(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 30, 9, 0, 0, 0, loc) // Monday 09:00 — 2 h left
	want := 2 * time.Hour
	if d := timeUntilPeakEnd(now, stdCfg); d != want {
		t.Errorf("timeUntilPeakEnd = %v, want %v", d, want)
	}
}

func TestTimeUntilPeakEnd_OutsidePeak_ReturnsZero(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 30, 12, 0, 0, 0, loc)
	if d := timeUntilPeakEnd(now, stdCfg); d != 0 {
		t.Errorf("timeUntilPeakEnd outside peak = %v, want 0", d)
	}
}

func TestTimeUntilPeakEnd_Weekend_ReturnsZero(t *testing.T) {
	loc := laLoc(t)
	now := time.Date(2026, 3, 28, 8, 0, 0, 0, loc) // Saturday — not peak
	if d := timeUntilPeakEnd(now, stdCfg); d != 0 {
		t.Errorf("timeUntilPeakEnd saturday = %v, want 0", d)
	}
}

// ---------------------------------------------------------------------------
// Timezone edge cases
// ---------------------------------------------------------------------------

// TestTimezone_SameInstantDifferentZones verifies that peak detection depends
// on the configured timezone, not UTC.
func TestTimezone_SameInstantDifferentZones(t *testing.T) {
	laLoc, _ := time.LoadLocation("America/Los_Angeles")
	utcLoc := time.UTC

	// Monday 2026-01-05 05:00 PST = 13:00 UTC.
	la5am := time.Date(2026, 1, 5, 5, 0, 0, 0, laLoc)

	// LA config: should be peak (05:00 PST is the start of window).
	laCfg := Config{Enabled: true, StartHour: 5, EndHour: 11, Timezone: "America/Los_Angeles"}
	if !isPeakAt(la5am, laCfg) {
		t.Error("expected peak at 05:00 LA time")
	}

	// UTC config with same 5–11 window: same instant is 13:00 UTC — not peak.
	utcCfg := Config{Enabled: true, StartHour: 5, EndHour: 11, Timezone: "UTC"}
	la5amInUTC := la5am.In(utcLoc)
	if isPeakAt(la5amInUTC, utcCfg) {
		t.Errorf("expected NOT peak at %v UTC (= 13:00)", la5amInUTC)
	}
}

// TestTimezone_DST_SpringForward verifies correctness across the LA DST
// boundary (clocks spring forward at 2 AM on the second Sunday of March).
// 2026-03-08 02:00 PST → 03:00 PDT; 05:00 PDT is still peak.
func TestTimezone_DST_SpringForward(t *testing.T) {
	loc, _ := time.LoadLocation("America/Los_Angeles")
	// Day before spring-forward (still PST)
	beforeDST := time.Date(2026, 3, 6, 8, 0, 0, 0, loc) // Friday
	// Day after (now PDT)
	afterDST := time.Date(2026, 3, 9, 8, 0, 0, 0, loc) // Monday
	if !isPeakAt(beforeDST, stdCfg) {
		t.Errorf("expected peak on pre-DST Friday: %v", beforeDST)
	}
	if !isPeakAt(afterDST, stdCfg) {
		t.Errorf("expected peak on post-DST Monday: %v", afterDST)
	}
}

// ---------------------------------------------------------------------------
// LoadConfig — file handling
// ---------------------------------------------------------------------------

func TestLoadConfig_MissingFile_ReturnsDefaults(t *testing.T) {
	t.Setenv("HOME", t.TempDir()) // directory has no .alluka/peak-hours.json
	cfg, err := LoadConfig()
	if err != nil {
		t.Fatalf("LoadConfig() unexpected error: %v", err)
	}
	if cfg != defaultConfig {
		t.Errorf("LoadConfig() = %+v, want %+v", cfg, defaultConfig)
	}
}

func TestLoadConfig_ValidFile(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	alluka := home + "/.alluka"
	if err := mkdirAll(alluka); err != nil {
		t.Fatal(err)
	}
	content := `{"enabled":false,"start_hour":6,"end_hour":10,"timezone":"UTC"}`
	if err := writeFile(alluka+"/peak-hours.json", content); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadConfig()
	if err != nil {
		t.Fatalf("LoadConfig() error: %v", err)
	}
	want := Config{Enabled: false, StartHour: 6, EndHour: 10, Timezone: "UTC"}
	if cfg != want {
		t.Errorf("LoadConfig() = %+v, want %+v", cfg, want)
	}
}

func TestLoadConfig_InvalidJSON_ReturnsError(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	alluka := home + "/.alluka"
	if err := mkdirAll(alluka); err != nil {
		t.Fatal(err)
	}
	if err := writeFile(alluka+"/peak-hours.json", `not json`); err != nil {
		t.Fatal(err)
	}

	if _, err := LoadConfig(); err == nil {
		t.Error("LoadConfig() expected error for invalid JSON, got nil")
	}
}

func TestLoadConfig_BadTimezone_ReturnsError(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	alluka := home + "/.alluka"
	if err := mkdirAll(alluka); err != nil {
		t.Fatal(err)
	}
	content := `{"enabled":true,"start_hour":5,"end_hour":11,"timezone":"Not/A/Zone"}`
	if err := writeFile(alluka+"/peak-hours.json", content); err != nil {
		t.Fatal(err)
	}

	if _, err := LoadConfig(); err == nil {
		t.Error("LoadConfig() expected error for bad timezone, got nil")
	}
}

// ---------------------------------------------------------------------------
// Helpers (avoid importing os directly in test assertions)
// ---------------------------------------------------------------------------

func mkdirAll(path string) error {
	return os.MkdirAll(path, 0o755)
}

func writeFile(path, content string) error {
	return os.WriteFile(path, []byte(content), 0o644)
}
