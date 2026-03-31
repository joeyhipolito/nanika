// Package config handles reading and writing the Obsidian CLI configuration file.
// Configuration is stored in ~/.obsidian/config in INI-style format.
// Set OBSIDIAN_CONFIG_DIR to override the default config directory location.
package config

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	// ConfigDir is the directory name for Obsidian CLI configuration.
	ConfigDir = ".obsidian"
	// ConfigFile is the configuration file name.
	ConfigFile = "config"
	// ConfigDirEnv is the environment variable that overrides the config directory.
	ConfigDirEnv = "OBSIDIAN_CONFIG_DIR"
)

// Config represents the Obsidian CLI configuration.
type Config struct {
	GeminiAPIKey string
	VaultPath    string
	WebsitePath  string
	WebsiteURL   string
}

// Store manages the obsidian config directory and file.
// It checks the configured env var to allow overriding the default location.
type Store struct {
	envVar string
}

// NewStoreWithEnv creates a Store that checks envVar for the config directory path.
// If the env var is set, its value is used as the config directory instead of ~/.obsidian.
// This preserves backward compatibility: callers without the env var behave identically.
func NewStoreWithEnv(envVar string) *Store {
	return &Store{envVar: envVar}
}

// defaultStore is used by package-level functions.
var defaultStore = NewStoreWithEnv(ConfigDirEnv)

// Dir returns the config directory path.
// If the env var is set, that path is returned; otherwise returns ~/.obsidian.
func (s *Store) Dir() (string, error) {
	if s.envVar != "" {
		if d := os.Getenv(s.envVar); d != "" {
			return d, nil
		}
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home directory: %w", err)
	}
	return filepath.Join(home, ConfigDir), nil
}

// Path returns the full path to the config file.
func (s *Store) Path() (string, error) {
	dir, err := s.Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, ConfigFile), nil
}

// Exists returns true if the config file exists.
func (s *Store) Exists() bool {
	p, err := s.Path()
	if err != nil {
		return false
	}
	_, err = os.Stat(p)
	return err == nil
}

// Permissions returns the file permissions of the config file.
func (s *Store) Permissions() (os.FileMode, error) {
	p, err := s.Path()
	if err != nil {
		return 0, err
	}
	info, err := os.Stat(p)
	if err != nil {
		return 0, fmt.Errorf("checking config permissions: %w", err)
	}
	return info.Mode().Perm(), nil
}

// Load reads the configuration from the config file.
// Returns an empty Config (not an error) if the file doesn't exist.
func (s *Store) Load() (*Config, error) {
	p, err := s.Path()
	if err != nil {
		return &Config{}, nil
	}

	f, err := os.Open(p)
	if err != nil {
		if os.IsNotExist(err) {
			return &Config{}, nil
		}
		return nil, fmt.Errorf("opening config: %w", err)
	}
	defer f.Close()

	cfg := &Config{}
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 {
			continue
		}
		key := strings.TrimSpace(parts[0])
		value := strings.TrimSpace(parts[1])
		switch key {
		case "gemini_apikey":
			cfg.GeminiAPIKey = value
		case "vault_path":
			cfg.VaultPath = value
		case "website_path":
			cfg.WebsitePath = value
		case "website_url":
			cfg.WebsiteURL = value
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading config: %w", err)
	}
	return cfg, nil
}

// Save writes the configuration to the config file with proper permissions.
func (s *Store) Save(cfg *Config) error {
	dir, err := s.Dir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("creating config directory: %w", err)
	}

	p := filepath.Join(dir, ConfigFile)

	var b strings.Builder
	b.WriteString("# Obsidian CLI Configuration\n")
	b.WriteString("# Created by: obsidian configure\n")
	b.WriteString("\n")
	b.WriteString("# Your Gemini API Key\n")
	b.WriteString("# Get from: https://aistudio.google.com/api-keys\n")
	fmt.Fprintf(&b, "gemini_apikey=%s\n", cfg.GeminiAPIKey)
	b.WriteString("\n")
	b.WriteString("# Path to your Obsidian vault\n")
	fmt.Fprintf(&b, "vault_path=%s\n", cfg.VaultPath)
	if cfg.WebsitePath != "" {
		b.WriteString("\n")
		b.WriteString("# Path to your website project (for obsidian sync)\n")
		fmt.Fprintf(&b, "website_path=%s\n", cfg.WebsitePath)
	}
	if cfg.WebsiteURL != "" {
		b.WriteString("\n")
		b.WriteString("# Base URL of your website (for obsidian sync links)\n")
		fmt.Fprintf(&b, "website_url=%s\n", cfg.WebsiteURL)
	}

	if err := os.WriteFile(p, []byte(b.String()), 0600); err != nil {
		return fmt.Errorf("writing config: %w", err)
	}
	return nil
}

// Package-level functions use defaultStore for backward compatibility.

// Path returns the full path to the config file (~/.obsidian/config).
// Respects OBSIDIAN_CONFIG_DIR if set.
func Path() string {
	p, _ := defaultStore.Path()
	return p
}

// Dir returns the full path to the config directory (~/.obsidian/).
// Respects OBSIDIAN_CONFIG_DIR if set.
func Dir() string {
	d, _ := defaultStore.Dir()
	return d
}

// Load reads the configuration from the config file.
// Returns an empty Config (not an error) if the file doesn't exist.
func Load() (*Config, error) {
	return defaultStore.Load()
}

// Save writes the configuration to the config file with proper permissions.
func Save(cfg *Config) error {
	return defaultStore.Save(cfg)
}

// Exists returns true if the config file exists.
func Exists() bool {
	return defaultStore.Exists()
}

// Permissions returns the file permissions of the config file, or an error.
func Permissions() (os.FileMode, error) {
	return defaultStore.Permissions()
}

// ResolveAPIKey returns the Gemini API key using config priority:
// config file > environment variable.
func ResolveAPIKey() string {
	cfg, err := Load()
	if err == nil && cfg.GeminiAPIKey != "" {
		return cfg.GeminiAPIKey
	}
	return os.Getenv("GEMINI_API_KEY")
}

// ResolveVaultPath returns the vault path from config or environment.
func ResolveVaultPath() string {
	cfg, err := Load()
	if err == nil && cfg.VaultPath != "" {
		return cfg.VaultPath
	}
	return os.Getenv("OBSIDIAN_VAULT_PATH")
}

// ResolveWebsitePath returns the website path from config or environment.
func ResolveWebsitePath() string {
	cfg, err := Load()
	if err == nil && cfg.WebsitePath != "" {
		return cfg.WebsitePath
	}
	return os.Getenv("OBSIDIAN_WEBSITE_PATH")
}

// ResolveWebsiteURL returns the website base URL from config or environment.
func ResolveWebsiteURL() string {
	cfg, err := Load()
	if err == nil && cfg.WebsiteURL != "" {
		return cfg.WebsiteURL
	}
	return os.Getenv("OBSIDIAN_WEBSITE_URL")
}
