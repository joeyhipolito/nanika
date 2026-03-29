// Package config handles reading and writing the scheduler configuration file.
// Configuration is stored in ~/.alluka/scheduler/config in key=value format.
package config

import (
	"bufio"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
)

const (
	// DirName is the directory under $HOME for scheduler config.
	DirName = ".alluka/scheduler"
	// FileName is the config file name.
	FileName = "config"
)

// Config holds all scheduler settings.
type Config struct {
	// DBPath is the path to the SQLite database file.
	DBPath string
	// LogLevel controls verbosity: "debug", "info", "warn", "error".
	LogLevel string
	// Shell is the shell used to execute job commands (default: /bin/sh).
	Shell string
	// MaxConcurrent limits simultaneous job executions (0 = unlimited).
	MaxConcurrent int
	// DashboardToken is a Bearer token required for POST/DELETE endpoints.
	DashboardToken string
}

// Default returns a Config populated with sensible defaults.
// DBPath is intentionally empty; callers set it based on their config directory.
func Default() *Config {
	return &Config{
		LogLevel:      "info",
		Shell:         "/bin/sh",
		MaxConcurrent: 4,
	}
}

// Dir returns the full path to the ~/.scheduler/ directory.
// If SCHEDULER_CONFIG_DIR is set, that path is used instead.
func Dir() string {
	if d := os.Getenv("SCHEDULER_CONFIG_DIR"); d != "" {
		return d
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ".scheduler"
	}
	return filepath.Join(home, DirName)
}

// Path returns the full path to the config file (~/.scheduler/config).
func Path() string {
	return filepath.Join(Dir(), FileName)
}

// Exists reports whether the config file exists.
func Exists() bool {
	_, err := os.Stat(Path())
	return err == nil
}

// EnsureDir creates ~/.scheduler/ if it doesn't exist.
func EnsureDir() error {
	d := Dir()
	if err := os.MkdirAll(d, 0700); err != nil {
		return fmt.Errorf("creating config dir %s: %w", d, err)
	}
	return nil
}

// Load reads and parses the config file. Missing keys use defaults.
// Returns the default config with any overrides from the file applied.
func Load() (*Config, error) {
	cfg := Default()
	cfg.DBPath = filepath.Join(Dir(), "scheduler.db")

	f, err := os.Open(Path())
	if os.IsNotExist(err) {
		return cfg, nil
	}
	if err != nil {
		return nil, fmt.Errorf("opening config %s: %w", Path(), err)
	}
	defer f.Close()

	if err := parseConfig(f, cfg); err != nil {
		return nil, err
	}
	return cfg, nil
}

// Save writes cfg to the config file, creating ~/.scheduler/ if needed.
// File permissions are set to 0600.
func Save(cfg *Config) error {
	if err := EnsureDir(); err != nil {
		return err
	}
	if err := os.WriteFile(Path(), []byte(formatConfig(cfg)), 0600); err != nil {
		return fmt.Errorf("writing config %s: %w", Path(), err)
	}
	return nil
}

// GenerateToken returns a cryptographically random 32-byte hex token.
func GenerateToken() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", fmt.Errorf("generating random token: %w", err)
	}
	return hex.EncodeToString(b), nil
}

// EnsureToken loads the config and generates a dashboard_token if missing.
// Returns the token. Saves the config if a new token was generated.
func EnsureToken() (string, error) {
	cfg, err := Load()
	if err != nil {
		return "", err
	}
	if cfg.DashboardToken != "" {
		return cfg.DashboardToken, nil
	}
	token, err := GenerateToken()
	if err != nil {
		return "", err
	}
	cfg.DashboardToken = token
	if err := Save(cfg); err != nil {
		return "", fmt.Errorf("saving config with new token: %w", err)
	}
	return token, nil
}

// Permissions returns the file permission bits of the config file.
func Permissions() (os.FileMode, error) {
	info, err := os.Stat(Path())
	if err != nil {
		return 0, fmt.Errorf("stat config: %w", err)
	}
	return info.Mode().Perm(), nil
}

// parseConfig applies key=value pairs from r into cfg.
// It is the single authoritative parser for the config format.
func parseConfig(r io.Reader, cfg *Config) error {
	scanner := bufio.NewScanner(r)
	lineNum := 0
	for scanner.Scan() {
		lineNum++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		k, v, ok := strings.Cut(line, "=")
		if !ok {
			return fmt.Errorf("config line %d: expected key=value, got %q", lineNum, line)
		}
		key := strings.TrimSpace(k)
		val := strings.TrimSpace(v)

		switch key {
		case "db_path":
			cfg.DBPath = val
		case "log_level":
			cfg.LogLevel = val
		case "shell":
			cfg.Shell = val
		case "max_concurrent":
			n := 0
			if _, err := fmt.Sscanf(val, "%d", &n); err != nil {
				return fmt.Errorf("config line %d: max_concurrent must be an integer, got %q", lineNum, val)
			}
			cfg.MaxConcurrent = n
		case "dashboard_token":
			cfg.DashboardToken = val
		default:
			// Unknown keys are silently ignored for forward compatibility.
		}
	}
	return scanner.Err()
}

// formatConfig serialises cfg to the key=value config file format.
func formatConfig(cfg *Config) string {
	lines := []string{
		"# scheduler configuration",
		"# Generated by 'scheduler-cli configure'",
		"",
		fmt.Sprintf("db_path = %s", cfg.DBPath),
		fmt.Sprintf("log_level = %s", cfg.LogLevel),
		fmt.Sprintf("shell = %s", cfg.Shell),
		fmt.Sprintf("max_concurrent = %d", cfg.MaxConcurrent),
		fmt.Sprintf("dashboard_token = %s", cfg.DashboardToken),
	}
	return strings.Join(lines, "\n") + "\n"
}

// Store manages scheduler configuration rooted at a specific directory.
// Use NewStoreWithEnv to create one in production; construct with a temp dir in tests.
type Store struct {
	dir string
}

// NewStoreWithEnv creates a Store whose directory is taken from the
// SCHEDULER_CONFIG_DIR environment variable, falling back to ~/.scheduler.
func NewStoreWithEnv() *Store {
	return &Store{dir: Dir()}
}

// newStoreWithDir creates a Store rooted at dir. Used in tests.
func newStoreWithDir(dir string) *Store {
	return &Store{dir: dir}
}

// Dir returns the config directory this Store is rooted at.
func (s *Store) Dir() string { return s.dir }

// Path returns the full path to the config file.
func (s *Store) Path() string { return filepath.Join(s.dir, FileName) }

// Exists reports whether the config file exists.
func (s *Store) Exists() bool {
	_, err := os.Stat(s.Path())
	return err == nil
}

// EnsureDir creates the config directory if it does not exist.
func (s *Store) EnsureDir() error {
	if err := os.MkdirAll(s.dir, 0700); err != nil {
		return fmt.Errorf("creating config dir %s: %w", s.dir, err)
	}
	return nil
}

// Load reads and parses the config file. Missing keys use defaults.
func (s *Store) Load() (*Config, error) {
	cfg := Default()
	cfg.DBPath = filepath.Join(s.dir, "scheduler.db")

	f, err := os.Open(s.Path())
	if os.IsNotExist(err) {
		return cfg, nil
	}
	if err != nil {
		return nil, fmt.Errorf("opening config %s: %w", s.Path(), err)
	}
	defer f.Close()

	if err := parseConfig(f, cfg); err != nil {
		return nil, err
	}
	return cfg, nil
}

// Save writes cfg to the config file, creating the directory if needed.
func (s *Store) Save(cfg *Config) error {
	if err := s.EnsureDir(); err != nil {
		return err
	}
	if err := os.WriteFile(s.Path(), []byte(formatConfig(cfg)), 0600); err != nil {
		return fmt.Errorf("writing config %s: %w", s.Path(), err)
	}
	return nil
}

// Permissions returns the file permission bits of the config file.
func (s *Store) Permissions() (os.FileMode, error) {
	info, err := os.Stat(s.Path())
	if err != nil {
		return 0, fmt.Errorf("stat config: %w", err)
	}
	return info.Mode().Perm(), nil
}
