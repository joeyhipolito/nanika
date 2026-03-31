// Package config handles reading and writing the Scout CLI configuration file.
// Configuration is stored in ~/.scout/config in INI-style format.
package config

import (
	"fmt"
	"os"
	"path/filepath"
)

const (
	// ConfigDirName is the directory name for Scout configuration.
	ConfigDirName = ".scout"
	// ConfigFileName is the configuration file name.
	ConfigFileName = "config"
	// TopicsDirName is the directory name for topic configurations.
	TopicsDirName = "topics"
	// IntelDirName is the directory name for gathered intel.
	IntelDirName = "intel"
)

var mgr = newWithEnv(ConfigDirName, ConfigFileName, "SCOUT_CONFIG_DIR")

// Config represents the Scout CLI configuration.
type Config struct {
	GatherInterval string
	GeminiAPIKey   string
}

// BaseDir returns the full path to the scout config base directory (~/.scout/).
func BaseDir() string {
	d, err := mgr.Dir()
	if err != nil {
		return ""
	}
	return d
}

// Path returns the full path to the config file (~/.scout/config).
func Path() string {
	p, err := mgr.Path()
	if err != nil {
		return ""
	}
	return p
}

// TopicsDir returns the full path to the topics directory (~/.scout/topics/).
func TopicsDir() string {
	base := BaseDir()
	if base == "" {
		return ""
	}
	return filepath.Join(base, TopicsDirName)
}

// IntelDir returns the full path to the intel directory (~/.scout/intel/).
func IntelDir() string {
	base := BaseDir()
	if base == "" {
		return ""
	}
	return filepath.Join(base, IntelDirName)
}

// HealthFile returns the full path to the health data file (~/.scout/health.json).
func HealthFile() string {
	base := BaseDir()
	if base == "" {
		return ""
	}
	return filepath.Join(base, "health.json")
}

// GatherStateFile returns the full path to the gather state file (~/.scout/gather-state.json).
func GatherStateFile() string {
	base := BaseDir()
	if base == "" {
		return ""
	}
	return filepath.Join(base, "gather-state.json")
}

// Load reads the configuration from ~/.scout/config.
// Returns an empty Config (not an error) if the file doesn't exist.
func Load() (*Config, error) {
	cfg := &Config{
		GatherInterval: "6h", // default
	}

	values, err := mgr.LoadINI()
	if err != nil {
		return nil, fmt.Errorf("failed to open config file: %w", err)
	}

	if v, ok := values["gather_interval"]; ok {
		cfg.GatherInterval = v
	}
	if v, ok := values["gemini_apikey"]; ok {
		cfg.GeminiAPIKey = v
	}

	return cfg, nil
}

// Save writes the configuration to ~/.scout/config with proper permissions.
func Save(cfg *Config) error {
	header := "# Scout CLI Configuration\n# Created by: scout configure\n\n# Default gather interval (e.g. 1h, 6h, 24h)"
	values := map[string]string{
		"gather_interval": cfg.GatherInterval,
		"gemini_apikey":   cfg.GeminiAPIKey,
	}
	keyOrder := []string{"gather_interval", "gemini_apikey"}
	if err := mgr.SaveINI(values, header, keyOrder); err != nil {
		return fmt.Errorf("failed to write config file: %w", err)
	}
	return nil
}

// Exists returns true if the config file exists.
func Exists() bool {
	path := Path()
	if path == "" {
		return false
	}
	_, err := os.Stat(path)
	return err == nil
}

// Permissions returns the file permissions of the config file, or an error.
func Permissions() (os.FileMode, error) {
	path := Path()
	if path == "" {
		return 0, fmt.Errorf("cannot determine config path")
	}
	info, err := os.Stat(path)
	if err != nil {
		return 0, err
	}
	return info.Mode().Perm(), nil
}

// EnsureDirs creates the topics and intel directories if they don't exist.
func EnsureDirs() error {
	dirs := []string{TopicsDir(), IntelDir()}
	for _, dir := range dirs {
		if dir == "" {
			return fmt.Errorf("cannot determine directory path")
		}
		if err := os.MkdirAll(dir, 0700); err != nil {
			return fmt.Errorf("failed to create directory %s: %w", dir, err)
		}
	}
	return nil
}
