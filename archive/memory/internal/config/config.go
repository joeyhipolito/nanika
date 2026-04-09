package config

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

const defaultStoreName = "default"

// defaultDecayHalfLifeDays is the default temporal decay half-life.
// An entry's score is halved after this many days.
const defaultDecayHalfLifeDays = 90.0

// BaseDir returns the root directory for memory data.
func BaseDir() string {
	if dir := strings.TrimSpace(os.Getenv("MEMORY_HOME")); dir != "" {
		return dir
	}
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return ".memory"
	}
	return filepath.Join(home, ".memory")
}

// StoreName returns the selected logical store name.
func StoreName() string {
	if name := strings.TrimSpace(os.Getenv("MEMORY_STORE")); name != "" {
		name = strings.ReplaceAll(name, string(os.PathSeparator), "-")
		if name != "" {
			return name
		}
	}
	return defaultStoreName
}

// StoreDir returns the absolute path to the active store directory.
func StoreDir() string {
	return filepath.Join(BaseDir(), StoreName())
}

// LogPath returns the append-only log location.
func LogPath() string {
	return filepath.Join(StoreDir(), "log.jsonl")
}

// SnapshotPath returns the compiled index snapshot location.
func SnapshotPath() string {
	return filepath.Join(StoreDir(), "compiled.gob")
}

// EnsureStoreDir creates the active store directory if missing.
func EnsureStoreDir() error {
	return os.MkdirAll(StoreDir(), 0o755)
}

// DecayHalfLifeDays returns the temporal decay half-life in days.
// Overridable via MEMORY_DECAY_HALF_LIFE_DAYS environment variable.
// Default is 90 days: an entry written 90 days ago scores half as much as
// an identical entry written today.
func DecayHalfLifeDays() float64 {
	if raw := strings.TrimSpace(os.Getenv("MEMORY_DECAY_HALF_LIFE_DAYS")); raw != "" {
		if days, err := strconv.ParseFloat(raw, 64); err == nil && days > 0 {
			return days
		}
	}
	return defaultDecayHalfLifeDays
}
