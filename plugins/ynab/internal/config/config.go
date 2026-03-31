// Package config handles reading and writing the YNAB CLI configuration file.
// The config directory defaults to ~/.ynab but can be overridden via YNAB_CONFIG_DIR.
package config

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	// ConfigDir is the directory name for YNAB configuration.
	ConfigDir = ".ynab"
	// ConfigFile is the configuration file name.
	ConfigFile = "config"
)

// store manages the config file at ~/.ynab/config (or $YNAB_CONFIG_DIR/config).
type store struct {
	dirName  string
	fileName string
	envVar   string
}

var s = &store{dirName: ConfigDir, fileName: ConfigFile, envVar: "YNAB_CONFIG_DIR"}

func (st *store) baseDir() (string, error) {
	if st.envVar != "" {
		if d := os.Getenv(st.envVar); d != "" {
			return d, nil
		}
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home directory: %w", err)
	}
	return filepath.Join(home, st.dirName), nil
}

func (st *store) path() (string, error) {
	base, err := st.baseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, st.fileName), nil
}

func (st *store) exists() bool {
	p, err := st.path()
	if err != nil {
		return false
	}
	_, err = os.Stat(p)
	return err == nil
}

func (st *store) permissions() (os.FileMode, error) {
	p, err := st.path()
	if err != nil {
		return 0, err
	}
	info, err := os.Stat(p)
	if err != nil {
		return 0, fmt.Errorf("checking config permissions: %w", err)
	}
	return info.Mode().Perm(), nil
}

func (st *store) load() (map[string]string, error) {
	p, err := st.path()
	if err != nil {
		return nil, err
	}
	f, err := os.Open(p)
	if err != nil {
		if os.IsNotExist(err) {
			return make(map[string]string), nil
		}
		return nil, fmt.Errorf("opening config: %w", err)
	}
	defer f.Close()

	values := make(map[string]string)
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
		values[strings.TrimSpace(parts[0])] = strings.TrimSpace(parts[1])
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading config: %w", err)
	}
	return values, nil
}

func (st *store) save(values map[string]string, header string, keyOrder []string) error {
	base, err := st.baseDir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(base, 0700); err != nil {
		return fmt.Errorf("creating config directory: %w", err)
	}

	p := filepath.Join(base, st.fileName)
	var sb strings.Builder
	if header != "" {
		sb.WriteString(header)
		if !strings.HasSuffix(header, "\n") {
			sb.WriteByte('\n')
		}
		sb.WriteByte('\n')
	}
	for _, key := range keyOrder {
		if val, ok := values[key]; ok {
			fmt.Fprintf(&sb, "%s=%s\n", key, val)
		}
	}
	if err := os.WriteFile(p, []byte(sb.String()), 0600); err != nil {
		return fmt.Errorf("writing config: %w", err)
	}
	return nil
}

// Config represents the YNAB CLI configuration.
type Config struct {
	AccessToken     string
	DefaultBudgetID string
	APIBaseURL      string
}

// Path returns the full path to the config file (~/.ynab/config or $YNAB_CONFIG_DIR/config).
// Returns "" if the path cannot be determined.
func Path() string {
	p, err := s.path()
	if err != nil {
		return ""
	}
	return p
}

// Dir returns the full path to the config directory.
// Returns "" if the path cannot be determined.
func Dir() string {
	d, err := s.baseDir()
	if err != nil {
		return ""
	}
	return d
}

// Load reads the configuration from the config file.
// Returns an empty Config (not an error) if the file doesn't exist.
func Load() (*Config, error) {
	values, err := s.load()
	if err != nil {
		return nil, fmt.Errorf("loading config: %w", err)
	}
	return &Config{
		AccessToken:     values["access_token"],
		DefaultBudgetID: values["default_budget_id"],
		APIBaseURL:      values["api_base_url"],
	}, nil
}

// Save writes the configuration to the config file with proper permissions.
func Save(cfg *Config) error {
	values := map[string]string{
		"access_token":      cfg.AccessToken,
		"default_budget_id": cfg.DefaultBudgetID,
	}
	if cfg.APIBaseURL != "" {
		values["api_base_url"] = cfg.APIBaseURL
	} else {
		values["api_base_url"] = "https://api.youneedabudget.com/v1"
	}

	header := "# YNAB CLI Configuration\n# Created by: ynab configure\n\n# Your YNAB Personal Access Token\n# Get from: https://app.ynab.com/settings/developer"
	keyOrder := []string{"access_token", "default_budget_id", "api_base_url"}
	return s.save(values, header, keyOrder)
}

// Exists returns true if the config file exists.
func Exists() bool {
	return s.exists()
}

// Permissions returns the file permissions of the config file, or an error.
func Permissions() (os.FileMode, error) {
	return s.permissions()
}

// ResolveToken returns the access token using config priority:
// config file > environment variable.
func ResolveToken() string {
	cfg, err := Load()
	if err == nil && cfg.AccessToken != "" {
		return cfg.AccessToken
	}
	return os.Getenv("YNAB_ACCESS_TOKEN")
}

// ResolveBudgetID returns the default budget ID from config or environment.
func ResolveBudgetID() string {
	cfg, err := Load()
	if err == nil && cfg.DefaultBudgetID != "" {
		return cfg.DefaultBudgetID
	}
	return os.Getenv("YNAB_DEFAULT_BUDGET_ID")
}
